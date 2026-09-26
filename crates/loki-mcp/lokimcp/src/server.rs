//! MCP server: exposes a Grafana Loki instance as read-only `LogQL` query and
//! label tools.

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

use crate::client::{LokiClient, seg};

/// MCP server wrapping a [`LokiClient`].
#[derive(Clone)]
pub struct LokiServer {
    client: LokiClient,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl LokiServer {
    /// Wrap a configured client as an MCP server.
    #[must_use]
    pub fn new(client: LokiClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }

    /// Run a GET against the Loki API and render the `data` payload as pretty
    /// JSON, mapping any error into an MCP error.
    async fn call(&self, path: &str, query: &[(&str, String)]) -> Result<String, ErrorData> {
        let data = self
            .client
            .get(path, query)
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        serde_json::to_string_pretty(&data).map_err(|e| {
            ErrorData::internal_error(format!("failed to serialize response: {e}"), None)
        })
    }
}

// ---- Tool parameter types ---------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct InstantQueryParams {
    /// `LogQL` expression, e.g. `{job="varlogs"} |= "error"` or a metric query
    /// like `count_over_time({job="app"}[5m])`.
    query: String,
    /// Evaluation time: RFC3339 or a Unix timestamp in nanoseconds. Defaults to now.
    #[serde(default)]
    time: Option<String>,
    /// Max number of entries to return (default: 100).
    #[serde(default)]
    limit: Option<u64>,
    /// Sort direction: `forward` or `backward` (default: `backward`, newest first).
    #[serde(default)]
    direction: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RangeQueryParams {
    /// `LogQL` expression to evaluate over the range.
    query: String,
    /// Range start: RFC3339 or Unix timestamp (nanoseconds). Defaults to one hour ago.
    #[serde(default)]
    start: Option<String>,
    /// Range end: RFC3339 or Unix timestamp (nanoseconds). Defaults to now.
    #[serde(default)]
    end: Option<String>,
    /// Max number of entries to return (default: 100).
    #[serde(default)]
    limit: Option<u64>,
    /// Step for metric queries, as a duration (`30s`, `5m`) or seconds.
    #[serde(default)]
    step: Option<String>,
    /// Sort direction: `forward` or `backward` (default: `backward`, newest first).
    #[serde(default)]
    direction: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LabelsParams {
    /// Optional start of the window to consider: RFC3339 or Unix ns. Defaults to 6h ago.
    #[serde(default)]
    start: Option<String>,
    /// Optional end of the window to consider: RFC3339 or Unix ns. Defaults to now.
    #[serde(default)]
    end: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LabelValuesParams {
    /// Label name to list values for, e.g. `job` or `app`.
    label: String,
    /// Optional start of the window to consider: RFC3339 or Unix ns.
    #[serde(default)]
    start: Option<String>,
    /// Optional end of the window to consider: RFC3339 or Unix ns.
    #[serde(default)]
    end: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SeriesParams {
    /// One or more log stream selectors, e.g. `["{job=\"varlogs\"}"]`.
    selectors: Vec<String>,
    /// Optional start: RFC3339 or Unix ns.
    #[serde(default)]
    start: Option<String>,
    /// Optional end: RFC3339 or Unix ns.
    #[serde(default)]
    end: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct IndexStatsParams {
    /// Log stream selector to compute stats for, e.g. `{job="varlogs"}`.
    query: String,
    /// Optional start: RFC3339 or Unix ns.
    #[serde(default)]
    start: Option<String>,
    /// Optional end: RFC3339 or Unix ns.
    #[serde(default)]
    end: Option<String>,
}

// ---- Tools ------------------------------------------------------------------

#[tool_router]
impl LokiServer {
    #[tool(
        description = "Run an instant LogQL query at a single point in time. Use for metric queries; for browsing log lines over a window prefer query_range."
    )]
    async fn query(
        &self,
        Parameters(InstantQueryParams {
            query,
            time,
            limit,
            direction,
        }): Parameters<InstantQueryParams>,
    ) -> Result<String, ErrorData> {
        let mut q = vec![
            ("query", query),
            ("limit", limit.unwrap_or(100).to_string()),
        ];
        if let Some(t) = time {
            q.push(("time", t));
        }
        if let Some(d) = direction {
            q.push(("direction", d));
        }
        self.call("/loki/api/v1/query", &q).await
    }

    #[tool(
        description = "Run a LogQL query over a time range — the main tool for searching logs (e.g. errors in a service over the last hour). Returns matching log streams or a metric matrix."
    )]
    async fn query_range(
        &self,
        Parameters(RangeQueryParams {
            query,
            start,
            end,
            limit,
            step,
            direction,
        }): Parameters<RangeQueryParams>,
    ) -> Result<String, ErrorData> {
        let mut q = vec![
            ("query", query),
            ("limit", limit.unwrap_or(100).to_string()),
        ];
        if let Some(s) = start {
            q.push(("start", s));
        }
        if let Some(e) = end {
            q.push(("end", e));
        }
        if let Some(s) = step {
            q.push(("step", s));
        }
        if let Some(d) = direction {
            q.push(("direction", d));
        }
        self.call("/loki/api/v1/query_range", &q).await
    }

    #[tool(
        description = "List all label names present within the time window (default: last 6 hours)."
    )]
    async fn labels(
        &self,
        Parameters(LabelsParams { start, end }): Parameters<LabelsParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(s) = start {
            q.push(("start", s));
        }
        if let Some(e) = end {
            q.push(("end", e));
        }
        self.call("/loki/api/v1/labels", &q).await
    }

    #[tool(
        description = "List all values for a given label name (e.g. all `job` or `app` values) within the time window."
    )]
    async fn label_values(
        &self,
        Parameters(LabelValuesParams { label, start, end }): Parameters<LabelValuesParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(s) = start {
            q.push(("start", s));
        }
        if let Some(e) = end {
            q.push(("end", e));
        }
        self.call(&format!("/loki/api/v1/label/{}/values", seg(&label)?), &q)
            .await
    }

    #[tool(
        description = "Find log streams matching one or more selectors, returning their label sets (not the log lines)."
    )]
    async fn series(
        &self,
        Parameters(SeriesParams {
            selectors,
            start,
            end,
        }): Parameters<SeriesParams>,
    ) -> Result<String, ErrorData> {
        let mut q: Vec<(&str, String)> = selectors.into_iter().map(|s| ("match[]", s)).collect();
        if let Some(s) = start {
            q.push(("start", s));
        }
        if let Some(e) = end {
            q.push(("end", e));
        }
        self.call("/loki/api/v1/series", &q).await
    }

    #[tool(
        description = "Get index statistics (stream, chunk, entry, and byte counts) for a selector — useful to gauge log volume before running a broad query."
    )]
    async fn index_stats(
        &self,
        Parameters(IndexStatsParams { query, start, end }): Parameters<IndexStatsParams>,
    ) -> Result<String, ErrorData> {
        let mut q = vec![("query", query)];
        if let Some(s) = start {
            q.push(("start", s));
        }
        if let Some(e) = end {
            q.push(("end", e));
        }
        self.call("/loki/api/v1/index/stats", &q).await
    }
}

// ---- Prompt arguments -------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ErrorScanArgs {
    /// Value of the stream label that selects the service, e.g. `varlogs` or
    /// `caddy`. Combined with `label` to form the stream selector.
    service: String,
    /// Stream label to match `service` against (default: `job`).
    #[serde(default)]
    label: Option<String>,
    /// `LogQL` duration for how far back to look, e.g. `1h`, `30m` (default: `1h`).
    #[serde(default)]
    window: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LabelExplorerArgs {
    /// Optional label to drill into. Omit to start from the full label list.
    #[serde(default)]
    label: Option<String>,
}

// ---- Prompts ----------------------------------------------------------------

/// Repeated Loki workflows, encoded as prompts.
///
/// These live in their own inherent `impl` block so `#[prompt_router]` and
/// `#[tool_router]` each own one block outright. Both generate an associated
/// router constructor (`Self::prompt_router()` / `Self::tool_router()`), and
/// keeping them separate avoids asking either macro to walk attributes it does
/// not recognise.
#[prompt_router]
impl LokiServer {
    /// Hunt for errors in one service's logs over a bounded window.
    #[prompt(
        name = "error_scan",
        description = "Scan one service's logs for errors over a bounded time window, then summarise what is failing."
    )]
    async fn error_scan(&self, params: Parameters<ErrorScanArgs>) -> Vec<PromptMessage> {
        let ErrorScanArgs {
            service,
            label,
            window,
        } = params.0;
        let label = label.unwrap_or_else(|| "job".to_string());
        let window = window.unwrap_or_else(|| "1h".to_string());

        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Scan the last {window} of logs for the service `{service}` and tell me what is \
                 going wrong.\n\n\
                 Work in this order:\n\
                 1. Call `label_values` for `{label}` and confirm `{service}` is actually a live \
                 stream value. If it is not, stop and say so rather than querying a selector that \
                 cannot match — an empty Loki result is indistinguishable from a healthy service.\n\
                 2. Call `query_range` with `{{{label}=\"{service}\"}} |~ \"(?i)error|fatal|panic|exception\"` \
                 over the last {window}. `query_range` is the workhorse here; the instant `query` \
                 tool answers a different question.\n\
                 3. If that returns nothing, widen once — drop the line filter and re-run to \
                 confirm the stream is producing logs at all. Distinguish 'no errors' from \
                 'no logs'.\n\
                 4. Call `index_stats` for the same selector and window to gauge volume, so you \
                 can say whether the sample you looked at is representative or truncated.\n\n\
                 Report: the distinct error signatures you found, roughly how often each occurs, \
                 the earliest and latest occurrence in the window, and whether the result was \
                 capped by the query limit. Quote real log lines. Do not speculate about causes \
                 that the logs do not support."
            ),
        )]
    }

    /// Walk the label namespace to build a working stream selector.
    #[prompt(
        name = "label_explorer",
        description = "Explore the label namespace to construct a valid LogQL stream selector before querying."
    )]
    async fn label_explorer(&self, params: Parameters<LabelExplorerArgs>) -> Vec<PromptMessage> {
        let opening = match params.0.label {
            Some(label) => format!(
                "Drill into the Loki label `{label}` and help me build a stream selector from it.\n\n\
                 Start by calling `label_values` for `{label}`."
            ),
            None => "Map out this Loki instance's label namespace so I can build a stream \
                     selector.\n\n\
                     Start by calling `labels` to list every label name."
                .to_string(),
        };

        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "{opening}\n\n\
                 Then:\n\
                 - Pick the labels that actually partition the data (`job`, `app`, `namespace`, \
                 `container` are typical) and call `label_values` on each. Ignore high-cardinality \
                 labels like `pod` or `instance` for selector-building — they fragment streams \
                 without helping you find anything.\n\
                 - Call `series` with a candidate selector to confirm it matches real streams \
                 before anyone spends a `query_range` on it.\n\
                 - Use `index_stats` on the candidate to show how much volume it covers.\n\n\
                 Finish by giving me two or three concrete, copy-pasteable `LogQL` selectors with \
                 a one-line note on what each one covers."
            ),
        )]
    }
}

// Rust 1.98's `clippy::unused_async_trait_impl` (pedantic, therefore deny here)
// fires four times on this block, and only two of them are ours: `list_resources`
// and `read_resource` genuinely have no `.await`. The other two originate inside
// the `tool_handler` and `prompt_handler` expansions -- rmcp generates async trait
// methods whose bodies are `std::future::ready(..)` -- so there is no source in
// this repo to change. Silencing it per-method would still leave the macro pair
// failing, which is why the allow sits on the whole impl. Revisit when rmcp stops
// generating bodies that never await; the two hand-written methods can drop their
// `async` at that point.
#[allow(clippy::unused_async_trait_impl)]
#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for LokiServer {
    fn get_info(&self) -> ServerConfig {
        // `ServerConfig` is `#[non_exhaustive]`, so build from default and assign.
        let mut info = ServerConfig::default();
        info.instructions = Some(
            "Read-only access to a Grafana Loki instance. Use these tools to search logs \
             with LogQL (query_range is the workhorse), inspect available labels/values, \
             and gauge log volume via index stats."
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
    ///
    /// The README carries the `LogQL` guidance and API caveats that no tool
    /// return value contains, so exposing it as a resource lets a client read
    /// the reasoning without spending a tool call on it.
    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let uri = mcp_common::doc_resource_uri(env!("CARGO_PKG_NAME"));
        Ok(ListResourcesResult::with_all_items(vec![
            mcp_common::doc_resource(
                &uri,
                "Loki MCP operator guide",
                "README for loki-mcp: LogQL usage, label conventions, and API caveats.",
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
