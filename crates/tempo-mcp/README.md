# tempo-mcp

MCP server for **Grafana Tempo**. Exposes read-only tools to search traces with
TraceQL and read individual traces, so an MCP client can answer "which requests
to service X were slow in the last hour — and *which span* was slow?".

Two crates:

- `tempomcp` — library: Tempo HTTP API client, trace compaction, MCP tool
  definitions and wiring.
- `tempomcp-server` — thin binary that serves the tools over streamable HTTP
  (axum), with the MCP endpoint mounted at `/mcp`.

The scope is deliberately **individual traces and TraceQL search**. Rate,
error and duration aggregates over the same spans are better answered from
Prometheus (prometheus-mcp), where Tempo's metrics-generator remote-writes
span-metrics and service-graph series. `metrics_query_range` is here for the
questions those series cannot answer (an ad-hoc attribute, a one-off
quantile), not as a replacement for them.

## Configuration

The server reads its configuration from environment variables. In this repo
they live encrypted in [`.sops.env`](../../.sops.env) at the root (sops dotenv
store — see the root README) and are injected by the `just` recipes:

```sh
sops .sops.env                 # edit (decrypts into $EDITOR, re-encrypts on save)
just watch-pkg tempomcp-server         # run with the variables injected
```

On startup the binary also loads a plain `.env` from the working directory
(via `dotenvy`) if one exists — copy [`.env.example`](../../.env.example) for
that. Real environment variables take precedence, and a missing `.env` is
fine (e.g. when an MCP client injects the variables itself).

| Variable              | Required | Default          | Description                                                            |
| --------------------- | -------- | ---------------- | --------------------------------------------------------------------- |
| `TEMPO_HOST`          | yes      | —                | `https://tempo.homelab.local`, `tempo.lan:3200`, or `tempo.lan`        |
| `TEMPO_TOKEN`         | no       | none             | Bearer token, sent as `Authorization: Bearer <token>` (use behind an auth proxy) |
| `TEMPO_ORG_ID`        | no       | none             | Tenant ID, sent as `X-Scope-OrgID` (required in multi-tenant mode)    |
| `TEMPO_INSECURE`      | no       | `false`          | Set `1`/`true` to accept self-signed TLS certificates                 |
| `TEMPO_BIND`          | no       | `127.0.0.1:8092` | Address to bind the streamable-HTTP server (endpoint at `/mcp`)       |
| `TEMPO_ALLOWED_HOSTS` | no       | loopback only    | Comma-separated allowed `Host` values; set when serving on a hostname |

If no scheme is given, `http://` is assumed; if no port is given, Tempo's
default HTTP port `3200` is used.

The default port is `8092` rather than the next free one after
alertmanager-mcp's `8086`: the homelab deployment allocated it before this
crate existed.

By default the server only accepts loopback `Host` headers (DNS-rebinding
protection). When exposing it on a hostname — e.g. behind Tailscale or a
reverse proxy — list that hostname in `TEMPO_ALLOWED_HOSTS`.

## Tools

All tools are read-only GETs against the
[Tempo HTTP API](https://grafana.com/docs/tempo/latest/api_docs/).

| Tool                  | Tempo endpoint                                        |
| --------------------- | ----------------------------------------------------- |
| `search`              | `GET /api/search`                                     |
| `trace`               | `GET /api/v2/traces/{traceID}`                        |
| `search_tags`         | `GET /api/v2/search/tags`                             |
| `search_tag_values`   | `GET /api/v2/search/tag/{tag}/values`                 |
| `metrics_query_range` | `GET /api/metrics/query_range`                        |
| `status`              | `GET /api/echo`, `GET /ready`, `GET /api/status/buildinfo` |

The usual flow is `search_tag_values` (is the service name real?) →
`search` (which traces match?) → `trace` (what happened inside one?).

### Time windows

Every `start`/`end` accepts **Unix seconds** or an **RFC3339** timestamp; the
server converts RFC3339 to the seconds Tempo requires. An integer that is
evidently milliseconds or nanoseconds (the units Loki and trace documents use)
is refused with an error naming the mistake, instead of being sent on: Tempo
would read it as a date tens of thousands of years away and answer with an
empty result indistinguishable from "nothing matched". `start` after `end` is
refused for the same reason.

**`search` without a window only sees recent data.** Tempo answers an unbounded
search from its ingesters alone, which hold minutes to hours of traces. To
search further back, pass `start` and `end`. `trace` is the opposite: without a
window it looks everywhere, and a window only narrows it.

### `search`

Returns trace **summaries**, never spans: ID, root service and span name, start
time, duration, the number of spans that matched the query, and Tempo's
per-service span/error counts when it provides them. `limit` defaults to 20;
`limitReached: true` means there are probably more. Tempo's `metrics` block is
passed through — `completedJobs < totalJobs` there means the search did not
cover everything.

### `trace` — the compact span tree

A trace is an OTLP document with every span, attribute and event; returned raw,
one lookup can fill a context window. By default `trace` returns instead:

- `spanCount`, `services` (spans per service), `rootService`/`rootName`,
  `startTime`, `durationMs`;
- `errorCount` and `errors` — each error span's ID, name, service and message
  (the span status message, or its `exception` event's type and message);
- `slowest` — the five longest spans;
- `spans` — a flat list in depth-first order (children by start time), each
  with `spanID`, `parentSpanID`, `depth`, `name`, `service`, `kind`,
  `startMs` (offset from the trace start), `durationMs`, `status`, an event
  count, and only the `attributes` you asked for (looked up on the span, then
  its resource).

`max_spans` (default 200) caps `spans`. When it cuts, the answer says so
(`truncated: true`, `omittedSpans`), and `errors`/`slowest` still cover the
whole trace, so the span that explains the problem is never the one dropped.
`raw: true` returns Tempo's document unchanged.

Span IDs are **hex**. Tempo's JSON encodes them as base64; they are converted
so an ID read here can be used in TraceQL (`{ span:id = "…" }`) and matches
what Grafana and logs show. A span whose parent is missing from the trace
(sampling, an unfinished ingest) is shown as a root at depth 0, with its
`parentSpanID` still set. A `PARTIAL` status from Tempo is surfaced as
`tempoStatus`/`tempoMessage`.

Trace IDs are validated (1–32 hex digits; `search` trims leading zeros) before
any request is made. A trace Tempo does not have is a tool error that says
where to look next, not an empty tree.

### `search_tags` / `search_tag_values`

`search_tags` lists attribute names per scope (`resource`, `span`,
`intrinsic`, `event`, `link`, `instrumentation`). `search_tag_values` takes the
**scoped** TraceQL name — `resource.service.name`, `span.http.route` — and an
optional `q` to restrict values to matching spans.

### `status`

`/api/echo` must answer, or the tool fails: it goes through the same proxy and
credentials as every other tool, so this is the check that the token works.
`/ready` and build info are reported per endpoint, since a reverse proxy may
expose only `/api/`. A wrong or missing token behind the homelab's Caddy
surfaces as `401 Unauthorized: (empty body)`.

## Running

Via the `justfile` (`just watch-pkg tempomcp-server`), with a `.env` in place, or by
setting the variables in the environment:

```sh
cargo run -p tempomcp-server
```

Or pass the variables inline:

```sh
TEMPO_HOST=http://tempo.lan:3200 cargo run -p tempomcp-server
```

The MCP endpoint is then served at `http://<TEMPO_BIND>/mcp` (default
`http://127.0.0.1:8092/mcp`). Set `RUST_LOG=debug` for verbose tracing.

### MCP client config

The server uses the streamable-HTTP transport, so point the client at its URL:

```json
{
  "mcpServers": {
    "tempo": {
      "type": "http",
      "url": "http://127.0.0.1:8092/mcp"
    }
  }
}
```
