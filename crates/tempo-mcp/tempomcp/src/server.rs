//! MCP server: exposes a Grafana Tempo instance as read-only trace lookup and
//! `TraceQL` search tools.

use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    Implementation, ListResourcesResult, PromptMessage, Role, ServerCapabilities, ServerConfig,
};
use rmcp::{
    ErrorData, ServerHandler, prompt, prompt_handler, prompt_router, schemars, tool, tool_handler,
    tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::TempoClient;
use crate::compact::{self, DEFAULT_MAX_SPANS, TraceView};
use crate::time::push_window;

/// Traces returned by `search` when the caller does not say. Tempo's own
/// default is 20 too, but it is configurable server-side; pinning it here
/// keeps the answer's size independent of how Tempo is deployed.
const DEFAULT_SEARCH_LIMIT: u64 = 20;

/// MCP server wrapping a [`TempoClient`].
#[derive(Clone)]
pub struct TempoServer {
    client: TempoClient,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl TempoServer {
    /// Wrap a configured client as an MCP server.
    #[must_use]
    pub fn new(client: TempoClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }

    /// Run a GET against the Tempo API, mapping any error into an MCP error.
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, ErrorData> {
        self.client
            .get_json(path, query)
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))
    }
}

fn pretty(v: &Value) -> Result<String, ErrorData> {
    serde_json::to_string_pretty(v)
        .map_err(|e| ErrorData::internal_error(format!("failed to serialize response: {e}"), None))
}

/// Validate a trace ID and normalise it to lowercase hex.
///
/// Tempo's IDs are up to 32 hex digits, and `search` returns them with leading
/// zeros trimmed, so anything from 1 to 32 hex digits is accepted. Everything
/// else (a base64 ID copied from raw OTLP, a UUID with dashes, `..`) is refused
/// before a request is made: sent through, it becomes a 400 from Tempo at best
/// and a request to a different path at worst.
fn trace_id(raw: &str) -> Result<String, ErrorData> {
    let id = raw.trim();
    if (1..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(id.to_ascii_lowercase())
    } else {
        Err(ErrorData::invalid_params(
            format!(
                "{raw:?} is not a trace ID: expected 1 to 32 hex digits, as `search` returns them"
            ),
            None,
        ))
    }
}

// ---- Tool parameter types ---------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchParams {
    /// `TraceQL` query selecting the traces to find, e.g.
    /// `{ resource.service.name = "hofvarpnir" && status = error }` or
    /// `{ span.http.route = "/api/download" && duration > 2s }`. `{}` matches
    /// every trace.
    q: String,
    /// Window start: Unix seconds or RFC3339. Without `start`/`end`, Tempo only
    /// searches recent data still held in its ingesters — pass a window to
    /// search older traces in the backend.
    #[serde(default)]
    start: Option<String>,
    /// Window end: Unix seconds or RFC3339.
    #[serde(default)]
    end: Option<String>,
    /// Max number of traces to return (default: 20).
    #[serde(default)]
    limit: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TraceParams {
    /// Trace ID in hex, as returned by `search` (leading zeros may be omitted).
    trace_id: String,
    /// Optional window start (Unix seconds or RFC3339) to narrow the lookup.
    /// Omit to search all of Tempo's retention.
    #[serde(default)]
    start: Option<String>,
    /// Optional window end (Unix seconds or RFC3339).
    #[serde(default)]
    end: Option<String>,
    /// Max spans to list in the tree (default: 200). The error and slowest-span
    /// summaries always cover the whole trace.
    #[serde(default)]
    max_spans: Option<usize>,
    /// Attribute keys to include on each span, looked up on the span and then
    /// its resource, e.g. `["http.route", "http.status_code", "db.statement"]`.
    /// Default: none.
    #[serde(default)]
    attributes: Vec<String>,
    /// Return Tempo's full OTLP JSON document instead of the compact tree.
    /// Large: every span, attribute and event. Default: false.
    #[serde(default)]
    raw: bool,
}

/// Which attribute scope `search_tags` lists.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum TagScope {
    Resource,
    Span,
    Intrinsic,
    Event,
    Link,
    Instrumentation,
}

impl TagScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Resource => "resource",
            Self::Span => "span",
            Self::Intrinsic => "intrinsic",
            Self::Event => "event",
            Self::Link => "link",
            Self::Instrumentation => "instrumentation",
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchTagsParams {
    /// Restrict to one attribute scope. Omit for all scopes.
    #[serde(default)]
    scope: Option<TagScope>,
    /// Optional `TraceQL` query: only list tags present on matching spans.
    #[serde(default)]
    q: Option<String>,
    /// Optional window start: Unix seconds or RFC3339.
    #[serde(default)]
    start: Option<String>,
    /// Optional window end: Unix seconds or RFC3339.
    #[serde(default)]
    end: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchTagValuesParams {
    /// Scoped `TraceQL` attribute to list values for, e.g.
    /// `resource.service.name`, `span.http.route`, or an intrinsic like `name`
    /// or `status`.
    tag: String,
    /// Optional `TraceQL` query: only list values seen on matching spans, e.g.
    /// `{ resource.service.name = "hofvarpnir" }`.
    #[serde(default)]
    q: Option<String>,
    /// Optional window start: Unix seconds or RFC3339.
    #[serde(default)]
    start: Option<String>,
    /// Optional window end: Unix seconds or RFC3339.
    #[serde(default)]
    end: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MetricsQueryRangeParams {
    /// `TraceQL` metrics query, e.g.
    /// `{ resource.service.name = "hofvarpnir" } | quantile_over_time(duration, .95) by (span.http.route)`
    /// or `{ status = error } | rate() by (resource.service.name)`.
    q: String,
    /// Range start: Unix seconds or RFC3339.
    #[serde(default)]
    start: Option<String>,
    /// Range end: Unix seconds or RFC3339.
    #[serde(default)]
    end: Option<String>,
    /// Step between samples as a duration, e.g. `60s` or `5m`. Defaults to
    /// Tempo's choice for the range.
    #[serde(default)]
    step: Option<String>,
}

// ---- Tools ------------------------------------------------------------------

#[tool_router]
impl TempoServer {
    #[tool(
        description = "Search for traces with TraceQL — the starting point for finding slow or failing requests. Returns trace summaries (ID, root service and span, start, duration, matched span count), never spans; follow up with `trace`. Without start/end only recent data is searched."
    )]
    async fn search(
        &self,
        Parameters(SearchParams {
            q,
            start,
            end,
            limit,
        }): Parameters<SearchParams>,
    ) -> Result<String, ErrorData> {
        let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        let mut query = vec![("q", q), ("limit", limit.to_string())];
        push_window(&mut query, start.as_deref(), end.as_deref())?;
        let doc = self.get("/api/search", &query).await?;
        pretty(&compact::search_summary(&doc, limit))
    }

    #[tool(
        description = "Fetch one trace by ID as a compact span tree: each span's ID, parent, depth, name, service, kind, start offset and duration in ms, and status, plus the trace's error spans and slowest spans. Pass `attributes` to include specific span/resource attributes, or `raw: true` for the full OTLP document."
    )]
    async fn trace(
        &self,
        Parameters(TraceParams {
            trace_id: raw_id,
            start,
            end,
            max_spans,
            attributes,
            raw,
        }): Parameters<TraceParams>,
    ) -> Result<String, ErrorData> {
        let id = trace_id(&raw_id)?;
        let mut query = Vec::new();
        push_window(&mut query, start.as_deref(), end.as_deref())?;
        let doc = self
            .client
            .get_json_or_404(&format!("/api/v2/traces/{id}"), &query)
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?
            .ok_or_else(|| {
                let within = if query.is_empty() {
                    "Check the ID (hex, as `search` returns it); traces older than Tempo's \
                     retention are deleted."
                } else {
                    "Check the ID (hex, as `search` returns it) and that start/end contain the \
                     trace, or omit them to search all of Tempo's retention."
                };
                ErrorData::internal_error(format!("trace {id} not found in Tempo. {within}"), None)
            })?;
        if raw {
            return pretty(&doc);
        }
        let view = TraceView {
            max_spans: max_spans.unwrap_or(DEFAULT_MAX_SPANS),
            attributes,
        };
        // Compact, not pretty: this is the one answer whose size scales with
        // the data, and indentation of a few hundred span entries costs more
        // tokens than their content.
        serde_json::to_string(&compact::trace_tree(&id, &doc, &view)).map_err(|e| {
            ErrorData::internal_error(format!("failed to serialize response: {e}"), None)
        })
    }

    #[tool(
        description = "List the attribute names (tags) available for TraceQL, grouped by scope (resource, span, intrinsic, …). Use before writing a query to learn what can be filtered on."
    )]
    async fn search_tags(
        &self,
        Parameters(SearchTagsParams {
            scope,
            q,
            start,
            end,
        }): Parameters<SearchTagsParams>,
    ) -> Result<String, ErrorData> {
        let mut query = Vec::new();
        if let Some(s) = scope {
            query.push(("scope", s.as_str().to_string()));
        }
        if let Some(q) = q {
            query.push(("q", q));
        }
        push_window(&mut query, start.as_deref(), end.as_deref())?;
        pretty(&self.get("/api/v2/search/tags", &query).await?)
    }

    #[tool(
        description = "List the values seen for one TraceQL attribute, e.g. every `resource.service.name` or `span.http.route`. Optionally filtered by a TraceQL query."
    )]
    async fn search_tag_values(
        &self,
        Parameters(SearchTagValuesParams { tag, q, start, end }): Parameters<SearchTagValuesParams>,
    ) -> Result<String, ErrorData> {
        let tag = mcp_common::path_segment(&tag)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let mut query = Vec::new();
        if let Some(q) = q {
            query.push(("q", q));
        }
        push_window(&mut query, start.as_deref(), end.as_deref())?;
        pretty(
            &self
                .get(&format!("/api/v2/search/tag/{tag}/values"), &query)
                .await?,
        )
    }

    #[tool(
        description = "Run a TraceQL metrics query over a time range (e.g. p95 duration or error rate by route, computed from spans). For standard RED metrics prefer the span-metrics series in Prometheus."
    )]
    async fn metrics_query_range(
        &self,
        Parameters(MetricsQueryRangeParams {
            q,
            start,
            end,
            step,
        }): Parameters<MetricsQueryRangeParams>,
    ) -> Result<String, ErrorData> {
        let mut query = vec![("q", q)];
        push_window(&mut query, start.as_deref(), end.as_deref())?;
        if let Some(s) = step {
            query.push(("step", s));
        }
        pretty(&self.get("/api/metrics/query_range", &query).await?)
    }

    #[tool(
        description = "Check that Tempo is reachable and ready, and report its version. Fails when the query API itself is unreachable or refuses the credentials."
    )]
    async fn status(&self, _: Parameters<mcp_common::NoArguments>) -> Result<String, ErrorData> {
        // `/api/echo` is the query API itself, behind the same proxy and auth
        // as every other tool: if it fails, nothing else will work, so that is
        // a tool error. `/ready` and build info are reported, not required —
        // a reverse proxy may expose only `/api/`.
        let echo = self
            .client
            .get_text("/api/echo")
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        let ready = match self.client.get_text("/ready").await {
            Ok(body) => json!({ "ok": body }),
            Err(e) => json!({ "error": format!("{e:#}") }),
        };
        let build = match self.client.get_json("/api/status/buildinfo", &[]).await {
            Ok(info) => json!({ "ok": info }),
            Err(e) => json!({ "error": format!("{e:#}") }),
        };
        pretty(&json!({ "echo": echo, "ready": ready, "buildinfo": build }))
    }
}

// ---- Prompt arguments -------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SlowTracesArgs {
    /// Value of `resource.service.name` for the service to investigate, e.g.
    /// `hofvarpnir`.
    service: String,
    /// `TraceQL` duration above which a request counts as slow, e.g. `500ms`,
    /// `2s` (default: `1s`).
    #[serde(default)]
    threshold: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ErrorTracesArgs {
    /// Value of `resource.service.name` for the service to investigate.
    service: String,
}

// ---- Prompts ----------------------------------------------------------------

/// Repeated Tempo workflows, encoded as prompts.
///
/// These live in their own inherent `impl` block so `#[prompt_router]` and
/// `#[tool_router]` each own one block outright (see lokimcp for the reason).
#[prompt_router]
impl TempoServer {
    /// Find where the time goes in a service's slow requests.
    #[prompt(
        name = "slow_traces",
        description = "Find a service's slow requests and identify which span, and which downstream service, the time is spent in."
    )]
    async fn slow_traces(&self, params: Parameters<SlowTracesArgs>) -> Vec<PromptMessage> {
        let SlowTracesArgs { service, threshold } = params.0;
        let threshold = threshold.unwrap_or_else(|| "1s".to_string());

        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Find out why requests to `{service}` are slow (over {threshold}).\n\n\
                 Work in this order:\n\
                 1. Call `search_tag_values` for `resource.service.name` and confirm `{service}` \
                 is a real value. If it is not, stop and say so — an empty search is \
                 indistinguishable from a fast service.\n\
                 2. Call `search` with `{{ resource.service.name = \"{service}\" && kind = server \
                 && duration > {threshold} }}` and an explicit `start`/`end` covering the last \
                 few hours — without a window Tempo only searches its most recent data.\n\
                 3. Pick the two or three slowest traces and call `trace` on each, with \
                 `attributes` set to whatever identifies the work (`http.route`, \
                 `db.statement`, `url.full` are typical).\n\
                 4. In each trace read `slowest`, then walk that span's `parentSpanID` chain in \
                 `spans` to see which call path it sits on. Distinguish time spent *in* a span \
                 from time spent waiting on its children.\n\n\
                 Report: the span (name and service) that dominates the latency, whether it is \
                 the same across traces, and the attribute values that distinguish slow \
                 requests. Quote real durations. If a trace came back `truncated`, say so."
            ),
        )]
    }

    /// Group a service's failing requests by what actually failed.
    #[prompt(
        name = "error_traces",
        description = "Find a service's failing requests and group them by the span and error that caused them."
    )]
    async fn error_traces(&self, params: Parameters<ErrorTracesArgs>) -> Vec<PromptMessage> {
        let service = params.0.service;

        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Find out what is failing in `{service}`.\n\n\
                 1. Call `search` with `{{ resource.service.name = \"{service}\" && status = error }}` \
                 over an explicit recent `start`/`end`.\n\
                 2. Call `trace` on up to five of the results and read each one's `errors` \
                 list. The first error in tree order is usually the cause; errors on its \
                 ancestors are usually propagation.\n\
                 3. If the failing span belongs to a different service than `{service}`, say \
                 so: the fault is downstream.\n\n\
                 Report the distinct error signatures (span name, service, message), how many \
                 of the traces you looked at show each, and one trace ID per signature so a \
                 human can open it. Do not speculate beyond what the spans say."
            ),
        )]
    }
}

// See lokimcp's server.rs: `clippy::unused_async_trait_impl` fires inside the
// `tool_handler`/`prompt_handler` expansions, which have no source here to
// change, and on the two resource methods, which never await.
#[allow(clippy::unused_async_trait_impl)]
#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for TempoServer {
    fn get_info(&self) -> ServerConfig {
        // `ServerConfig` is `#[non_exhaustive]`, so build from default and assign.
        let mut info = ServerConfig::default();
        info.instructions = Some(
            "Read-only access to a Grafana Tempo instance. Find traces with TraceQL \
             (`search`, with an explicit start/end), then read one with `trace`, which \
             returns a compact span tree with its errors and slowest spans. Use \
             `search_tags`/`search_tag_values` to learn what can be queried. Aggregate \
             rates and latencies are better answered from span-metrics in Prometheus."
                .to_string(),
        );
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_prompts()
            .enable_resources()
            .build();

        let mut server_info = Implementation::default();
        server_info.name = env!("CARGO_PKG_NAME").to_string();
        server_info.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info = server_info;

        info
    }

    /// Advertise this crate's README as the server's operator guide.
    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let uri = mcp_common::doc_resource_uri(env!("CARGO_PKG_NAME"));
        Ok(ListResourcesResult::with_all_items(vec![
            mcp_common::doc_resource(
                &uri,
                "Tempo MCP operator guide",
                "README for tempo-mcp: TraceQL usage, time windows, the compact trace format, \
                 and API caveats.",
            ),
        ]))
    }

    /// Serve the operator guide's markdown for the URI advertised above.
    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, ErrorData> {
        let uri = mcp_common::doc_resource_uri(env!("CARGO_PKG_NAME"));
        if request.uri == uri {
            Ok(mcp_common::doc_resource_contents(&uri, include_str!("../../README.md")).into())
        } else {
            Err(ErrorData::resource_not_found(
                format!("unknown resource uri: {}", request.uri),
                None,
            ))
        }
    }
}
