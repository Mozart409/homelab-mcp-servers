# tempo-mcp: new MCP server for Grafana Tempo

**Severity:** feature — the only homelab telemetry store with no MCP server
**Crate:** `crates/tempo-mcp/{tempomcp,tempomcp-server}` (does not exist yet)
**Raised:** 2026-09-25, while wiring hofvarpnir's trace export back up

## Why

Prometheus, Loki and Alertmanager each have an MCP server aggregated by
axon-gateway, so an agent can ask about metrics, logs and alerts. Tempo has
none — traces are the one signal only a human with a Grafana tab can read.
hofvarpnir is about to start exporting spans again, which makes that gap the
difference between "an agent can tell you *that* a download was slow" and "an
agent can tell you *which span* was slow".

Scope it deliberately: **individual traces and TraceQL search**. The RED view
of the same spans is already answerable through `prom_query`, because Tempo's
metrics-generator remote-writes span-metrics and service-graph series into
Prometheus. Do not reimplement rate/latency/error aggregation here.

## Consumer wiring is already done

The homelab repo (`pve-nixos-homelab`) is already written for this server and
is waiting on the package. Nothing needs to be requested there; it switches
itself on at the first `nix flake update homelab-mcp` after this lands:

- `hosts/mcp_vm/configuration.nix` — `tempomcp-server` instance, guarded by
  `hasTempoMcp = mcpPackages ? tempomcp-server`. Binds `127.0.0.1:8092`,
  `TEMPO_HOST = https://tempo.homelab.local`, token = `otel-query-token`.
- `hosts/mcp_vm/axon-gateway/default.nix` — the `tempo` backend, guarded by
  the instance existing. Tools surface as `tempo_*`.

**This constrains the contract below.** The instance name, the binary name,
the port and the env prefix are already chosen; changing them means changing
two files in the other repo too.

## What to build

### 1. Crate layout — copy `crates/loki-mcp`

`loki-mcp` is the closest model: read-only HTTP API, bearer token, behind an
authenticating reverse proxy with a step-ca certificate.

```
crates/tempo-mcp/tempomcp/          # lib: config.rs, client.rs, server.rs, lib.rs
crates/tempo-mcp/tempomcp-server/   # bin, [[bin]] name = "tempomcp-server"
```

The workspace globs `crates/*/*`, so no `members` edit is needed.

Reuse `lokimcp::client`'s shape verbatim: `mcp_common::install_crypto_provider()`,
the sensitive `Authorization` header baked into `default_headers`, the
`danger_accept_invalid_certs(insecure)` escape hatch, `mcp_common::normalize_base_url`
(default port **3200**), `mcp_common::parse_allowed_hosts`. That reqwest setup
already talks to `https://loki.homelab.local` with a step-ca leaf, which is
exactly the TLS situation `tempo.homelab.local` presents — do not invent a new
client and rediscover it.

### 2. Env contract (`TEMPO_*`)

| Var | Required | Default |
|---|---|---|
| `TEMPO_HOST` | yes | — |
| `TEMPO_TOKEN` | no | none (sent as `Authorization: Bearer …`) |
| `TEMPO_BIND` | no | `127.0.0.1:8092` |
| `TEMPO_ALLOWED_HOSTS` | no | rmcp loopback-only default |
| `TEMPO_INSECURE` | no | `false` |

### 3. Flake: two edits, both easy to forget

- `packages.tempomcp-server = mkServer "tempomcp-server";` (next to
  `lokimcp-server`, ~line 87).
- **`knownServers.tempomcp-server = { prefix = "TEMPO"; port = 8092; hasToken = true; };`**
  in `nixosModules.default` (~line 345). `serverDefaults` falls back to
  `lib.toUpper type` for unknown server types, so without this entry the NixOS
  module exports `TEMPOMCP-SERVER_HOST` / `TEMPOMCP-SERVER_BIND` and the server
  starts with no configuration at all — and `from_env()` fails on missing
  `TEMPO_HOST`, i.e. it fails loudly but for a confusing reason. This is the
  single most likely thing to get wrong.

### 4. Tools

Tempo's query API, one tool per endpoint. `start`/`end` are unix **seconds**.

| Tool | Endpoint | Notes |
|---|---|---|
| `search` | `GET /api/search?q=&start=&end=&limit=&spss=` | `q` is TraceQL. Returns trace summaries, not spans. |
| `trace` | `GET /api/v2/traces/{traceID}?start=&end=` | The one that matters. See size note below. |
| `search_tags` | `GET /api/v2/search/tags?scope=&start=&end=` | `scope` ∈ resource/span/intrinsic. |
| `search_tag_values` | `GET /api/v2/search/tag/{tag}/values?q=&start=&end=` | Percent-encode `{tag}` — dotted names like `service.name` are the norm and `pbs-mcp` has already been bitten by unencoded path segments. |
| `metrics_query_range` | `GET /api/metrics/query_range?q=&start=&end=&step=` | TraceQL metrics. |
| `status` | `GET /api/echo`, `GET /ready` | Cheap liveness for the gateway's own health view. |

Prefer the `v2` endpoints where they exist; Tempo 3.x is what is deployed.

### 5. Response size is the real design problem

A single trace is an OTLP JSON document with every span, every attribute and
every event. This goes to an LLM through the gateway. Returning it raw is how
you burn a context window on one download.

- `trace` should default to a **compact span tree** — span id, parent, name,
  service, start offset in ms, duration, status, and only the attributes that
  were asked for — with `raw: true` as an opt-in for the full OTLP document.
- Cap spans returned (`max_spans`, default a few hundred) and say in the
  response when the result was truncated, rather than silently cutting.
- `search` should default to a small `limit` (20) and never return spans.

### 6. Tests

`wiremock`, same as the other servers: one test per tool asserting the URL,
query params and percent-encoding, plus a `from_env` test with the `ENV_LOCK`
serialization idiom from `lokimcp::config::tests`.

## Acceptance

- `tempo_search` with `q = '{ resource.service.name = "hofvarpnir" }'` returns
  trace IDs through axon-gateway.
- `tempo_trace` on one of those IDs returns a readable span tree well under a
  context window.
- Wrong/absent token gives a clean error, not a panic — `tempo.homelab.local`
  answers `401` from Caddy, not from Tempo.
