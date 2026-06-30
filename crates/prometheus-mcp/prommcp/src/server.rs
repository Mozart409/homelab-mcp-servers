//! MCP server: exposes a Prometheus instance as read-only query and status tools.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::client::PromClient;

/// MCP server wrapping a [`PromClient`].
#[derive(Clone)]
pub struct PromServer {
    client: PromClient,
    tool_router: ToolRouter<Self>,
}

impl PromServer {
    /// Wrap a configured client as an MCP server.
    #[must_use]
    pub fn new(client: PromClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
        }
    }

    /// Run a GET against the Prometheus API and render the `data` payload as
    /// pretty JSON, mapping any error into an MCP error.
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
    /// `PromQL` expression to evaluate, e.g. `up` or `rate(http_requests_total[5m])`.
    query: String,
    /// Evaluation timestamp: RFC3339 (`2026-06-30T12:00:00Z`) or a Unix
    /// timestamp in seconds. Defaults to the server's current time.
    #[serde(default)]
    time: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RangeQueryParams {
    /// `PromQL` expression to evaluate over the range.
    query: String,
    /// Range start: RFC3339 or Unix timestamp (seconds).
    start: String,
    /// Range end: RFC3339 or Unix timestamp (seconds).
    end: String,
    /// Resolution step, as a duration (`30s`, `5m`, `1h`) or seconds.
    step: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SeriesParams {
    /// One or more series selectors, e.g. `["up", "process_cpu_seconds_total{job=\"node\"}"]`.
    selectors: Vec<String>,
    /// Optional start: RFC3339 or Unix timestamp (seconds).
    #[serde(default)]
    start: Option<String>,
    /// Optional end: RFC3339 or Unix timestamp (seconds).
    #[serde(default)]
    end: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LabelValuesParams {
    /// Label name to list values for, e.g. `job` or `instance`.
    label: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TargetsParams {
    /// Filter by target state: `active`, `dropped`, or `any` (default `any`).
    #[serde(default)]
    state: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RulesParams {
    /// Filter by rule type: `alert` or `record` (default: both).
    #[serde(default)]
    rule_type: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MetadataParams {
    /// Restrict metadata to a single metric name (default: all metrics).
    #[serde(default)]
    metric: Option<String>,
}

// ---- Tools ------------------------------------------------------------------

#[tool_router]
impl PromServer {
    #[tool(
        description = "Evaluate a PromQL expression at a single instant. Returns the current value(s) of the query (a vector or scalar)."
    )]
    async fn query(
        &self,
        Parameters(InstantQueryParams { query, time }): Parameters<InstantQueryParams>,
    ) -> Result<String, ErrorData> {
        let mut q = vec![("query", query)];
        if let Some(t) = time {
            q.push(("time", t));
        }
        self.call("/api/v1/query", &q).await
    }

    #[tool(
        description = "Evaluate a PromQL expression over a time range, returning a time series matrix. Provide start, end, and step."
    )]
    async fn query_range(
        &self,
        Parameters(RangeQueryParams {
            query,
            start,
            end,
            step,
        }): Parameters<RangeQueryParams>,
    ) -> Result<String, ErrorData> {
        let q = vec![
            ("query", query),
            ("start", start),
            ("end", end),
            ("step", step),
        ];
        self.call("/api/v1/query_range", &q).await
    }

    #[tool(
        description = "Find series matching one or more selectors, returning the label sets of matching series (not their values)."
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
        self.call("/api/v1/series", &q).await
    }

    #[tool(description = "List all label names present in the Prometheus database.")]
    async fn labels(&self) -> Result<String, ErrorData> {
        self.call("/api/v1/labels", &[]).await
    }

    #[tool(
        description = "List all values for a given label name (e.g. all `job` or `instance` values)."
    )]
    async fn label_values(
        &self,
        Parameters(LabelValuesParams { label }): Parameters<LabelValuesParams>,
    ) -> Result<String, ErrorData> {
        self.call(&format!("/api/v1/label/{label}/values"), &[])
            .await
    }

    #[tool(
        description = "List scrape targets and their health (up/down), last scrape time, and labels. Filter by state: active, dropped, or any."
    )]
    async fn targets(
        &self,
        Parameters(TargetsParams { state }): Parameters<TargetsParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(s) = state {
            q.push(("state", s));
        }
        self.call("/api/v1/targets", &q).await
    }

    #[tool(
        description = "List currently active alerts with their state (pending/firing), labels, and annotations."
    )]
    async fn alerts(&self) -> Result<String, ErrorData> {
        self.call("/api/v1/alerts", &[]).await
    }

    #[tool(
        description = "List configured alerting and recording rules with their state and health. Filter by type: alert or record."
    )]
    async fn rules(
        &self,
        Parameters(RulesParams { rule_type }): Parameters<RulesParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(t) = rule_type {
            q.push(("type", t));
        }
        self.call("/api/v1/rules", &q).await
    }

    #[tool(
        description = "Get metric metadata (type, help text, unit). Optionally restrict to a single metric name."
    )]
    async fn metadata(
        &self,
        Parameters(MetadataParams { metric }): Parameters<MetadataParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(m) = metric {
            q.push(("metric", m));
        }
        self.call("/api/v1/metadata", &q).await
    }

    #[tool(
        description = "Get TSDB stats: head series/chunks, label cardinality, and per-metric series counts."
    )]
    async fn tsdb_status(&self) -> Result<String, ErrorData> {
        self.call("/api/v1/status/tsdb", &[]).await
    }

    #[tool(
        description = "Get Prometheus build information: version, revision, branch, build date, and Go version."
    )]
    async fn build_info(&self) -> Result<String, ErrorData> {
        self.call("/api/v1/status/buildinfo", &[]).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PromServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`, so build from default and assign.
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Read-only access to a Prometheus instance. Use these tools to run PromQL \
             queries (instant and range), inspect series/labels, and check target health, \
             alerts, and rules."
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
