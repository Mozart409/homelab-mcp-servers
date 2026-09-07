# alertmanager-mcp

MCP server exposing a [Prometheus Alertmanager](https://prometheus.io/docs/alerting/latest/alertmanager/)
instance over the streamable-HTTP transport, mounted at `/mcp`.

Prometheus can tell you that an alert is firing. It cannot tell you what
happened to the notification afterwards — which route matched, which receiver
was chosen, and whether the alert was silenced, inhibited, muted, or actually
delivered. That is what this server reads.

- Library crate: `alertmanagermcp`
- Binary crate: `alertmanagermcp-server`
- Default port: `8086`

## Configuration

All settings come from environment variables, loaded from `.env` via `dotenvy`.

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `ALERTMANAGER_HOST` | yes | — | Host or full URL. Scheme defaults to `http`, port to `9093`. |
| `ALERTMANAGER_TOKEN` | no | none | Sent as `Authorization: Bearer <token>`. |
| `ALERTMANAGER_INSECURE` | no | `false` | Accept invalid/self-signed TLS certificates. |
| `ALERTMANAGER_BIND` | no | `127.0.0.1:8086` | Bind address. Must be `0.0.0.0:8086` inside a container. |
| `ALERTMANAGER_ALLOWED_HOSTS` | no | loopback only | Comma-separated `Host` allow-list. |
| `ALERTMANAGER_ALLOW_SILENCE` | no | `false` | Register the two silence-write tools. |

The homelab instance is <https://alertmanager.homelab.internal>:

```sh
ALERTMANAGER_HOST=https://alertmanager.homelab.internal
```

Give the **full URL including the scheme**. `ALERTMANAGER_HOST` only defaults to
`http` and port `9093` when neither is present, so a bare
`alertmanager.homelab.internal` would be normalised to
`http://alertmanager.homelab.internal:9093` and fail — this instance serves TLS
on 443.

## Tools

### Read-only — always available

| Tool | Endpoint | Notes |
| --- | --- | --- |
| `list_alerts` | `GET /api/v2/alerts` | Alerts as Alertmanager sees them, including `silencedBy` / `inhibitedBy` / `mutedBy`. |
| `alert_groups` | `GET /api/v2/alerts/groups` | Alerts grouped by the routing tree, with the resolved receiver. |
| `list_silences` | `GET /api/v2/silences` | Matchers, window, creator, comment, state. |
| `get_silence` | `GET /api/v2/silence/{id}` | One silence by ID. |
| `list_receivers` | `GET /api/v2/receivers` | Configured receiver names. |
| `status` | `GET /api/v2/status` | Version, uptime, cluster state, loaded config. |

`list_alerts` shows *whether* an alert is suppressed; `alert_groups` is the only
view that shows *where* it would have been routed. Use both when auditing a
notification.

An alert's `status` block carries **three** independent suppression reasons —
`silencedBy`, `inhibitedBy`, and `mutedBy` (a time interval configured in the
routing tree). They have different causes and different fixes, so never collapse
them into "suppressed".

### Silence writes — gated off by default

| Tool | Endpoint |
| --- | --- |
| `create_silence` | `POST /api/v2/silences` |
| `expire_silence` | `DELETE /api/v2/silence/{id}` |

These are registered **only** when `ALERTMANAGER_ALLOW_SILENCE` is set to
`1`/`true`/`yes`. With the variable unset they do not appear in `tools/list` at
all, and the server describes itself as read-only.

This is the workspace's second deliberate exception to the read-only rule (see
AGENTS.md, Hard rules §1). It defaults off because a silence is homelab-wide
alert blindness with a time limit — enabling it should be a deliberate
deployment act, not a side effect of pulling an image.

`create_silence` refuses an empty matcher list. Alertmanager would accept one
and silence *every* alert; requiring at least one matcher means that outcome
cannot be reached by omitting an argument.

## Matcher syntax

Both the `filter` parameters and silence matchers use Prometheus label-matcher
syntax:

```
alertname="NodeDown"       # equality
severity=~"critical|page"  # regex
job!="node-exporter"       # negated equality
instance!~"test-.*"        # negated regex
```

`filter` is repeatable — pass several matchers and they are ANDed.

## Silence lifecycle

Silences are never deleted, only expired. `expire_silence` sets the end time to
now; the silence stays visible in `list_silences` with state `expired`. A
silence therefore has three states — `pending` (start time in the future),
`active`, and `expired` — and "I deleted it" is not one of them.

## API caveats

- **No response envelope.** Unlike the Prometheus HTTP API, Alertmanager v2
  returns bare JSON arrays and objects. There is no `{"status", "data"}` wrapper.
- **Singular and plural paths.** The collection is `/api/v2/silences` (list,
  create) but a single silence is `/api/v2/silence/{id}` (get, expire).
- **v1 is gone.** `/api/v1/alerts` answers `410 Gone` on 0.33.1; this server
  targets v2 only.
- **Error bodies have no single shape.** Verified against Alertmanager 0.33.1: a
  malformed silence ID returns `422` with a JSON body
  (`{"code":601,"message":"silenceID in path must be of type uuid: ..."}`), a
  well-formed but unknown silence ID returns `404` with an **empty** body, and a
  proxy in front may return HTML. This is exactly why the client checks the
  status before parsing rather than inferring failure from a parse error.
- **Silence IDs are UUIDs**, and Alertmanager validates the format server-side —
  hence the `422` above rather than a `404` for a non-UUID string.

## Re-verifying against the live instance

Every caveat above was checked against <https://alertmanager.homelab.internal>
running Alertmanager **0.33.1**. The v2 API is OpenAPI-generated, so an upgrade
can change response shapes without changing a path. After upgrading, re-run
these and compare:

```sh
B=https://alertmanager.homelab.internal

# Every endpoint this server calls should answer 200.
for p in /api/v2/status /api/v2/receivers /api/v2/silences \
         /api/v2/alerts /api/v2/alerts/groups; do
  printf '%-24s %s\n' "$p" "$(curl -s -o /dev/null -w '%{http_code}' "$B$p")"
done

# Bare JSON, no {"status","data"} envelope.
curl -s "$B/api/v2/receivers" | head -c 200

# Suppression reasons: silencedBy / inhibitedBy / mutedBy must all still exist.
curl -s "$B/api/v2/alerts" | jq '.[0].status'

# Silence matchers must still be camelCase isRegex / isEqual.
curl -s "$B/api/v2/silences" | jq '.[0].matchers'

# Error shapes: 422 + JSON for a malformed id, 404 + empty body for an unknown one.
curl -s -w '\n%{http_code}\n' "$B/api/v2/silence/not-a-uuid"
curl -s -w '\n%{http_code}\n' "$B/api/v2/silence/00000000-0000-4000-8000-000000000000"
```

If any of those change, the tests in `src/server.rs` encode the old expectations
and will need updating alongside the client.

## Prompts

- `notification_audit` — trace an alert through grouping and routing to a
  receiver, separating silenced from inhibited from muted from delivered.
- `silence_review` — audit silences for over-broad matchers, stale entries, and
  expiries landing on still-firing alerts.

## Development

```sh
just test-pkg alertmanagermcp
just watch-pkg alertmanagermcp-server
just image alertmanagermcp-server
```
