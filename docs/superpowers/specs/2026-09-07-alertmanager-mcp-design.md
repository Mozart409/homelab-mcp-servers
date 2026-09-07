# Alertmanager MCP — design

Date: 2026-09-07
Status: approved, implementing on `feat/alertmanager-mcp`

## Why

Prometheus tells you an alert is firing. It cannot tell you what happened to the
notification afterwards: which route matched, which receiver was picked, whether
the alert was silenced, inhibited, or genuinely delivered. That is Alertmanager's
half of the story, and today nothing in this workspace can read it.

The homelab runs Alertmanager **0.33.1** (job `alertmanager`, instance
`homelab-otel`, single-node — `alertmanager_cluster_enabled=0`), currently
carrying 1 active and 9 suppressed alerts and notifying through the `webhook`
integration. The suppressed count is the point: nine alerts are being held back
and Prometheus cannot say why.

## Scope

A seventh server crate, `alertmanager-mcp`, exposing Alertmanager's v2 HTTP API.
Six read-only tools always; two silence-write tools behind a default-off env
gate.

Out of scope: `POST /api/v2/alerts` (injecting alerts), the removed v1 API,
Alertmanager configuration mutation, and cluster/peer administration.

## Naming and placement

```
crates/alertmanager-mcp/
  alertmanagermcp/         config.rs, client.rs, server.rs, lib.rs
  alertmanagermcp-server/  thin main
  README.md
```

Port **8086** (next free after wp `8085`). Env prefix `ALERTMANAGER_`.

The prefix is spelled out rather than abbreviated. `AM_` is opaque, and unlike
`WP_` there is no collision to route around — nothing else in the deployment
claims the `ALERTMANAGER_` namespace.

## Hard rule §1: the second exception

AGENTS.md currently states that `homeassistant-mcp` is *the* exception to the
read-only rule, and that "the exception is per-server, not a precedent." This
server makes that sentence false, so the rule is rewritten rather than quietly
bent.

The new condition, which hamcp is grandfathered out of: **a mutating tool
outside hamcp must be gated behind an env var that defaults to off.** Writing
this down is what keeps a third case from being waved through on the strength of
the second.

### Why silences are worth the exception

Creating a silence is the one Alertmanager action that a read-only server makes
frustrating rather than merely limited — you can see that nine alerts are
suppressed and that a tenth is about to page you during planned maintenance, and
then have to leave the tool to do anything about it.

### Why the gate defaults to off

A silence is homelab-wide alert blindness with a time limit. Enabling it should
be a deliberate deployment act, not something that arrives because an image got
pulled. hamcp needs no such gate because control *is* its purpose; here the
mutating surface is two tools out of eight.

## Configuration

All settings from `ALERTMANAGER_*`, loaded from `.env` via `dotenvy`.

| Variable | Required | Default | Notes |
| --- | --- | --- | --- |
| `ALERTMANAGER_HOST` | yes | — | Scheme defaults to `http`, port to `9093` |
| `ALERTMANAGER_TOKEN` | no | none | Sent as `Authorization: Bearer` |
| `ALERTMANAGER_INSECURE` | no | `false` | Accept invalid TLS certs |
| `ALERTMANAGER_BIND` | no | `127.0.0.1:8086` | |
| `ALERTMANAGER_ALLOWED_HOSTS` | no | loopback only | Comma-separated |
| `ALERTMANAGER_ALLOW_SILENCE` | no | `false` | Enables the two write tools |

`normalize_base_url` and `parse_allowed_hosts` follow prommcp's shape, including
the rule that an allow-list which parses to empty collapses to `None` rather
than to `Some(vec![])` — the latter would reject every inbound `Host` header and
produce a server that silently accepts nothing.

### Known duplication, deliberately deferred

`prommcp` and `lokimcp` already carry near-identical `config.rs` files, and this
crate makes a third copy of `normalize_base_url` / `parse_allowed_hosts`.
Lifting them into `mcp-common` is worth doing but would touch two working
servers in a branch that is already introducing a new one and rewriting a hard
rule. Filed as `todos/lift-config-helpers-into-mcp-common.md`.

## Client

`AlertmanagerClient` — `get`, `post_json`, `delete`, cheap to clone.

The one real departure from `PromClient`: **the v2 API returns bare JSON.** There
is no `{"status": "success", "data": ...}` envelope to unwrap, so `get()` returns
the parsed body directly. Copying prommcp's unwrap verbatim would silently
return `null` for every call.

Failures arrive as a plain-text or JSON body with a 4xx/5xx status, so a shared
`ensure_success` helper checks the status *before* the body is parsed and carries
status plus a truncated body into the error. This exists because of the bug
documented in hamcp's client — a 500 parsed into a defaulted value and reported
as a successful no-op. Silence creation is exactly where that failure mode would
be worst: the tool would report a silence that does not exist.

Retains from prommcp: `mcp_common::install_crypto_provider()` before the first
client build (mandatory under `rustls-no-provider`), optional bearer token marked
sensitive, `danger_accept_invalid_certs`, and `seg()` percent-encoding for
interpolated path segments.

## Tools

### Read-only (always registered)

| Tool | Endpoint |
| --- | --- |
| `list_alerts` | `GET /api/v2/alerts` — `active`, `silenced`, `inhibited`, `unprocessed`, `filter[]`, `receiver` |
| `alert_groups` | `GET /api/v2/alerts/groups` — same filters, grouped by route |
| `list_silences` | `GET /api/v2/silences` — `filter[]` |
| `get_silence` | `GET /api/v2/silence/{id}` |
| `list_receivers` | `GET /api/v2/receivers` |
| `status` | `GET /api/v2/status` — cluster, config, uptime, version |

### Silence writes (registered only when `ALERTMANAGER_ALLOW_SILENCE` is set)

| Tool | Endpoint |
| --- | --- |
| `create_silence` | `POST /api/v2/silences` |
| `expire_silence` | `DELETE /api/v2/silence/{id}` |

`create_silence` takes a typed body: `matchers[{name, value, isRegex, isEqual}]`,
`startsAt`, `endsAt`, `createdBy`, `comment`.

**Path gotcha:** plural `/silences` for list and create, singular `/silence/{id}`
for get and expire. This is an easy transposition and is covered by a test.

### Gate mechanics

Two named routers — `#[tool_router(router = read_tools)]` and
`#[tool_router(router = silence_tools)]` — merged in `AlertmanagerServer::new`
only when the flag is on. With the gate off the write tools are **absent from
`tools/list`**, not present-and-failing: a tool the client can see is a tool the
model will try, and discovering the gate by calling into an error is worse than
never seeing it.

Verified against rmcp 3.1.4: the `tool_router` macro accepts a `router` ident,
and `ToolRouter` exposes `merge`.

## Prompts

`notification_audit` — trace an alert from firing through group and route to a
receiver, and separate *silenced* from *inhibited* from *actually delivered*.

`silence_review` — audit active and pending silences: flag over-broad matchers,
and flag silences about to expire on alerts that are still firing.

Both deliberately target what Prometheus structurally cannot answer, which is
what keeps this server from duplicating prommcp's `alert_triage`.

## Error handling

Tool methods map client errors into `ErrorData::internal_error` with `{e:#}` so
the eyre chain survives. A 404 from `get_silence` on an unknown ID is a normal
outcome and must surface as a clear message rather than an opaque decode error.

## Testing

`wiremock` integration tests in the lib crate's `tests/`, following prommcp's
"match `any()` so a wrong request still gets served" pattern.

- Bare-JSON parsing: no envelope unwrap, no `null` returns.
- Gate off ⇒ the two write tools are absent from the router; gate on ⇒ present.
- Singular vs plural silence paths.
- Non-2xx is never read as success.
- `get_silence` 404 surfaces cleanly.
- Config tests under the `ENV_LOCK` + `clear_env` pattern, including
  `normalize_base_url` defaulting to `:9093` and `ALLOW_SILENCE` parsing.

## Integration touch points

- `flake.nix` — `serverPkgs` entry, and a `knownServers` entry
  (`prefix = "ALERTMANAGER"`, `port = 8086`, `hasToken = true`).
  The release workflow reads the server list from `packages`, so the new image
  joins releases automatically.
- `just image-all` — one line.
- `compose.yaml` — service on 8086, `ALERTMANAGER_BIND: 0.0.0.0:8086`.
- `.env.example` — new block.
- Root `README.md` — servers table row, a description section, and the
  `mcpServers` snippet.
- `crates/alertmanager-mcp/README.md` — new, and exposed as the server's MCP
  doc resource.
- `AGENTS.md` — server list, port list, and the Hard rule §1 rewrite.

## Verification

The design was drafted against Alertmanager's documented v2 OpenAPI spec, then
checked against the live instance at <https://alertmanager.homelab.internal>
(0.33.1) before the branch was finished. Confirmed:

- All five endpoints this server calls answer `200`; `/api/v1/alerts` answers
  `410 Gone`, so the v2-only decision holds.
- Responses are bare JSON — `[...]` for receivers and silences, `{...}` for
  status. No `{"status","data"}` envelope anywhere, which is the assumption the
  client is built on.
- Silence matchers on the wire use exactly `name`, `value`, `isRegex`, `isEqual`
  — matching what `create_silence` sends.
- The singular `/api/v2/silence/{id}` path is real; `/api/v2/silences/{id}`
  answers `404`.

Three things the spec got wrong, corrected in the implementation:

1. **Alerts have a third suppression reason.** `status` carries `mutedBy`
   (time-interval muting) alongside `silencedBy` and `inhibitedBy`. The
   `notification_audit` prompt originally told the model to sort alerts into
   three buckets and would have silently misfiled every muted alert; it now
   sorts into four.
2. **Error bodies have no single shape.** A malformed silence ID returns `422`
   with a *JSON* body, and a well-formed but unknown one returns `404` with an
   *empty* body. The README's "errors are not JSON" claim was wrong. Both shapes
   are now covered by tests, including the `<empty body>` fallback.
3. **Silence IDs are UUIDs**, validated server-side — which is why a non-UUID is
   a `422` rather than a `404`.

Because the v2 API is OpenAPI-generated, an upgrade can change response shapes
without changing a path. The crate README carries a re-verification script
pinned to the same URL; run it after upgrading Alertmanager.
