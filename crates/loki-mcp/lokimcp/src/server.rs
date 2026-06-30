//! MCP server: exposes a Grafana Loki instance as read-only `LogQL` query and
//! label tools.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::client::LokiClient;

/// MCP server wrapping a [`LokiClient`].
#[derive(Clone)]
pub struct LokiServer {
    client: LokiClient,
    tool_router: ToolRouter<Self>,
}

impl LokiServer {
    /// Wrap a configured client as an MCP server.
    #[must_use]
    pub fn new(client: LokiClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
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
struct LabelsParams {
    /// Optional start of the window to consider: RFC3339 or Unix ns. Defaults to 6h ago.
    #[serde(default)]
    start: Option<String>,
    /// Optional end of the window to consider: RFC3339 or Unix ns. Defaults to now.
    #[serde(default)]
    end: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
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
        self.call(&format!("/loki/api/v1/label/{label}/values"), &q)
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

#[tool_handler(router = self.tool_router)]
impl ServerHandler for LokiServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`, so build from default and assign.
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Read-only access to a Grafana Loki instance. Use these tools to search logs \
             with LogQL (query_range is the workhorse), inspect available labels/values, \
             and gauge log volume via index stats."
                .to_string(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();

        let mut server_info = Implementation::default();
        server_info.name = env!("CARGO_PKG_NAME").to_string();
        server_info.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info = server_info;

        info
    }
}
