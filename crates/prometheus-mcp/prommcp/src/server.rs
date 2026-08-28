//! MCP server: exposes a Prometheus instance as read-only query and status tools.

use std::fmt::Write;

use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    Implementation, ListResourcesResult, PromptMessage, Role, ServerCapabilities, ServerInfo,
};
use rmcp::{
    ErrorData, ServerHandler, prompt, prompt_handler, prompt_router, schemars, tool, tool_handler,
    tool_router,
};
use serde::Deserialize;

use crate::client::{PromClient, seg};

/// MCP server wrapping a [`PromClient`].
#[derive(Clone)]
pub struct PromServer {
    client: PromClient,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl PromServer {
    /// Wrap a configured client as an MCP server.
    #[must_use]
    pub fn new(client: PromClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
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

// ---- Prompt arguments -------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AlertTriageArgs {
    /// Alert severity to filter on, e.g. `critical` or `warning`. Omit to show all severities.
    #[serde(default)]
    severity: Option<String>,
    /// Optional `PromQL` label matcher to narrow scope, e.g. `job="api"` to filter by job.
    #[serde(default)]
    matcher: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TargetHealthArgs {
    /// Restrict to a single scrape job. Omit to check all jobs.
    #[serde(default)]
    job: Option<String>,
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
        self.call(&format!("/api/v1/label/{}/values", seg(&label)), &[])
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

// ---- Prompts ----------------------------------------------------------------

/// Repeated Prometheus workflows, encoded as prompts.
///
/// These live in their own inherent `impl` block so `#[prompt_router]` and
/// `#[tool_router]` each own one block outright. Both generate an associated
/// router constructor (`Self::prompt_router()` / `Self::tool_router()`), and
/// keeping them separate avoids asking either macro to walk attributes it does
/// not recognise.
#[prompt_router]
impl PromServer {
    /// Triage active alerts: walk the model through their rules, run queries to ground
    /// reasoning in actual data, and distinguish real breaches from scrape failures.
    #[prompt(
        name = "alert_triage",
        description = "Investigate active alerts: identify their root causes, compare rule definitions against live data, and rank by severity and trend."
    )]
    async fn alert_triage(&self, params: Parameters<AlertTriageArgs>) -> Vec<PromptMessage> {
        let AlertTriageArgs { severity, matcher } = params.0;

        let mut instructions = String::from(
            "Triage the active alerts in this Prometheus instance and tell me what is failing.\n\n\
             Work in this order:\n\
             1. Call `alerts` to get what is currently firing. ",
        );

        if let Some(s) = &severity {
            let _ = write!(instructions, "Filter for severity `{s}` ");
        } else {
            instructions.push_str("Scan all severities. ");
        }

        if let Some(m) = &matcher {
            let _ = write!(instructions, "in alerts matching `{m}`. ");
        }

        instructions.push_str(
            "Note the label sets of each distinct alert.\n\
             2. For each distinct alert, call `rules` to recover the alert rule's expression and `for` \
             duration — do not guess from the alert name; anchor reasoning in the rule definition.\n\
             3. Run `query` or `query_range` on the rule's own expression to see how far over \
             threshold it is and whether the metric is trending toward or away from resolution. Use \
             `query_range` with a 10-15 minute window to detect trend.\n\
             4. Call `targets` to check whether the alert is caused by a scrape failure rather than \
             a genuine application breach — a down target (`up{instance=\"...\"}=0`) produces alerts \
             that look like application failures but are infrastructure issues, not data issues.\n\n\
             Report: \
             - Group alerts by likely common cause rather than listing them flat.\n\
             - Distinguish a real breach from a scrape/staleness artifact.\n\
             - State for each whether it is worsening, steady, or recovering based on the range query \
             rather than the instantaneous value.\n\
             - Quote the rule definition and the most recent metric value."
        );

        vec![PromptMessage::new_text(Role::User, instructions)]
    }

    /// Walk the model through scrape health to diagnose why targets are failing,
    /// whether scrapes are slow, and whether cardinality pressure is degrading collection.
    #[prompt(
        name = "target_health",
        description = "Diagnose scrape health: find which targets are down, struggling, or misconfigured."
    )]
    async fn target_health(&self, params: Parameters<TargetHealthArgs>) -> Vec<PromptMessage> {
        let TargetHealthArgs { job } = params.0;

        let mut instructions = String::from(
            "Diagnose the scrape health of this Prometheus instance.\n\n\
             Work in this order:\n\
             1. Call `targets` to list up/down state and `lastError`/`lastScrape` metadata per target. ",
        );

        if let Some(j) = &job {
            let _ = write!(instructions, "Restrict to the `{j}` scrape job. ");
        }

        instructions.push_str(
            "Note which targets are down (`state=dropped` or `up=0`) and their error signatures.\n\
             2. For each target that is down or scraping slowly (large `lastScrape` interval), query \
             `up{instance=\"...\"}` with `query_range` over the last 30 minutes to confirm whether \
             the target is perpetually down or intermittently failing.\n\
             3. Call `tsdb_status` to check Prometheus's own health: series/chunk cardinality, \
             head-block series count, and which metrics dominate. Use `metadata` to name the \
             high-cardinality metrics and gauge whether dropping them would help.\n\
             4. Call `build_info` to confirm the Prometheus version (some versions have known scrape \
             bugs).\n\n\
             Report: \
             - A verdict distinguishing (a) targets genuinely down with their last error, (b) targets up \
             but scraping slowly or erroring intermittently, and (c) a Prometheus instance under cardinality \
             pressure that is degrading scrapes on its own.\n\
             - Flag high-cardinality metrics from `tsdb_status` when they are the actual problem, not \
             target health.\n\
             - For each down target, quote the rule or config that should be collecting from it."
        );

        vec![PromptMessage::new_text(Role::User, instructions)]
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
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
    /// The README carries `PromQL` guidance and Prometheus HTTP API caveats that no
    /// tool return value contains, so exposing it as a resource lets a client read
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
                "Prometheus MCP operator guide",
                "README for prometheus-mcp: PromQL usage, scrape config, and API caveats.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::PromClient;
    use crate::config::Config;
    use color_eyre::eyre::{Result, eyre};
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    /// Start a mock Prometheus that answers *every* request — any method, any
    /// path — with `template`, and point a [`PromServer`] at it.
    ///
    /// Matching on `any()` rather than on a method/path is deliberate: a request
    /// the code got wrong still gets served, so the assertions below can report
    /// what was actually sent instead of failing with an opaque 404.
    async fn mock_prom(template: ResponseTemplate) -> Result<(MockServer, PromServer)> {
        let mock = MockServer::start().await;
        Mock::given(any()).respond_with(template).mount(&mock).await;

        let config = Config {
            base_url: mock.uri(),
            token: None,
            insecure: false,
            bind: "127.0.0.1:0".to_string(),
            allowed_hosts: None,
        };
        let server = PromServer::new(PromClient::new(&config)?);
        Ok((mock, server))
    }

    /// A minimal well-formed Prometheus success envelope.
    fn ok_body() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_string(r#"{"status":"success","data":{}}"#)
    }

    /// The one request the mock recorded, or an error describing what it saw.
    async fn only_request(mock: &MockServer) -> Result<Request> {
        let mut requests = mock
            .received_requests()
            .await
            .ok_or_else(|| eyre!("mock server is not recording requests"))?;
        if requests.len() != 1 {
            return Err(eyre!("expected exactly 1 request, got {}", requests.len()));
        }
        requests.pop().ok_or_else(|| eyre!("no request recorded"))
    }

    /// Flatten a tool's `ErrorData` into `eyre` so tests can use `?`.
    fn ok(result: Result<String, ErrorData>) -> Result<String> {
        result.map_err(|e| eyre!("tool returned an error: {e:?}"))
    }

    #[tokio::test]
    async fn query_omits_time_when_unset() -> Result<()> {
        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server
            .query(Parameters(InstantQueryParams {
                query: "up".to_string(),
                time: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/v1/query");
        assert_eq!(req.url.query(), Some("query=up"));
        Ok(())
    }

    #[tokio::test]
    async fn query_sends_time_when_set() -> Result<()> {
        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server
            .query(Parameters(InstantQueryParams {
                query: "up".to_string(),
                time: Some("42".to_string()),
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/v1/query");
        assert_eq!(req.url.query(), Some("query=up&time=42"));
        Ok(())
    }

    #[tokio::test]
    async fn query_range_sends_all_four_required_params() -> Result<()> {
        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server
            .query_range(Parameters(RangeQueryParams {
                query: "up".to_string(),
                start: "1".to_string(),
                end: "2".to_string(),
                step: "30s".to_string(),
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/v1/query_range");
        // Unlike Loki's, this tool takes no `limit` and no optionals at all.
        assert_eq!(req.url.query(), Some("query=up&start=1&end=2&step=30s"));
        Ok(())
    }

    #[tokio::test]
    async fn series_repeats_the_match_param_and_omits_unset_window() -> Result<()> {
        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server
            .series(Parameters(SeriesParams {
                selectors: vec!["up".to_string(), "down".to_string()],
                start: None,
                end: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/v1/series");
        assert_eq!(req.url.query(), Some("match%5B%5D=up&match%5B%5D=down"));
        Ok(())
    }

    #[tokio::test]
    async fn series_sends_the_window_when_set() -> Result<()> {
        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server
            .series(Parameters(SeriesParams {
                selectors: vec!["up".to_string()],
                start: Some("1".to_string()),
                end: Some("2".to_string()),
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.query(), Some("match%5B%5D=up&start=1&end=2"));
        Ok(())
    }

    #[tokio::test]
    async fn labels_sends_no_query() -> Result<()> {
        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server.labels().await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/v1/labels");
        assert_eq!(req.url.query(), None);
        Ok(())
    }

    #[tokio::test]
    async fn label_values_percent_encodes_the_label() -> Result<()> {
        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server
            .label_values(Parameters(LabelValuesParams {
                label: "foo/bar".to_string(),
            }))
            .await)?;

        let req = only_request(&mock).await?;
        // The `/` must survive as `%2F` rather than adding a path segment.
        assert_eq!(req.url.path(), "/api/v1/label/foo%2Fbar/values");
        assert_eq!(req.url.query(), None);
        Ok(())
    }

    #[tokio::test]
    async fn targets_sends_state_only_when_set() -> Result<()> {
        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server
            .targets(Parameters(TargetsParams { state: None }))
            .await)?;
        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/v1/targets");
        assert_eq!(req.url.query(), None);

        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server
            .targets(Parameters(TargetsParams {
                state: Some("active".to_string()),
            }))
            .await)?;
        let req = only_request(&mock).await?;
        assert_eq!(req.url.query(), Some("state=active"));
        Ok(())
    }

    #[tokio::test]
    async fn rules_maps_rule_type_onto_the_type_param() -> Result<()> {
        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server
            .rules(Parameters(RulesParams { rule_type: None }))
            .await)?;
        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/v1/rules");
        assert_eq!(req.url.query(), None);

        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server
            .rules(Parameters(RulesParams {
                rule_type: Some("alert".to_string()),
            }))
            .await)?;
        let req = only_request(&mock).await?;
        // The MCP-facing name is `rule_type`; Prometheus wants `type`.
        assert_eq!(req.url.query(), Some("type=alert"));
        Ok(())
    }

    #[tokio::test]
    async fn metadata_sends_metric_only_when_set() -> Result<()> {
        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server
            .metadata(Parameters(MetadataParams { metric: None }))
            .await)?;
        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/v1/metadata");
        assert_eq!(req.url.query(), None);

        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server
            .metadata(Parameters(MetadataParams {
                metric: Some("up".to_string()),
            }))
            .await)?;
        let req = only_request(&mock).await?;
        assert_eq!(req.url.query(), Some("metric=up"));
        Ok(())
    }

    #[tokio::test]
    async fn parameterless_tools_hit_their_fixed_paths() -> Result<()> {
        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server.alerts().await)?;
        assert_eq!(only_request(&mock).await?.url.path(), "/api/v1/alerts");

        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server.tsdb_status().await)?;
        assert_eq!(only_request(&mock).await?.url.path(), "/api/v1/status/tsdb");

        let (mock, server) = mock_prom(ok_body()).await?;
        ok(server.build_info().await)?;
        assert_eq!(
            only_request(&mock).await?.url.path(),
            "/api/v1/status/buildinfo"
        );
        Ok(())
    }

    #[tokio::test]
    async fn upstream_500_maps_to_an_error() -> Result<()> {
        let (_mock, server) = mock_prom(
            ResponseTemplate::new(500).set_body_string(r#"{"status":"error","error":"boom"}"#),
        )
        .await?;

        assert!(
            server.labels().await.is_err(),
            "a 500 must surface as ErrorData, not a panic"
        );
        Ok(())
    }

    #[tokio::test]
    async fn malformed_json_maps_to_an_error() -> Result<()> {
        let (_mock, server) =
            mock_prom(ResponseTemplate::new(200).set_body_string("not json at all")).await?;

        assert!(
            server.labels().await.is_err(),
            "a malformed body must surface as ErrorData, not a panic"
        );
        Ok(())
    }

    #[tokio::test]
    async fn malformed_json_on_a_500_maps_to_an_error() -> Result<()> {
        // The client parses the body before checking the status, so this path
        // must still fail rather than reporting success.
        let (_mock, server) =
            mock_prom(ResponseTemplate::new(500).set_body_string("<html>oops</html>")).await?;

        assert!(server.labels().await.is_err());
        Ok(())
    }

    /// Executable form of AGENTS.md hard rule §1: this server is read-only, so
    /// every tool must reach Prometheus with a `GET` and nothing else.
    #[tokio::test]
    async fn every_tool_issues_only_get_requests() -> Result<()> {
        let (mock, server) = mock_prom(ok_body()).await?;

        ok(server
            .query(Parameters(InstantQueryParams {
                query: "up".to_string(),
                time: None,
            }))
            .await)?;
        ok(server
            .query_range(Parameters(RangeQueryParams {
                query: "up".to_string(),
                start: "1".to_string(),
                end: "2".to_string(),
                step: "30s".to_string(),
            }))
            .await)?;
        ok(server
            .series(Parameters(SeriesParams {
                selectors: vec!["up".to_string()],
                start: None,
                end: None,
            }))
            .await)?;
        ok(server.labels().await)?;
        ok(server
            .label_values(Parameters(LabelValuesParams {
                label: "job".to_string(),
            }))
            .await)?;
        ok(server
            .targets(Parameters(TargetsParams { state: None }))
            .await)?;
        ok(server.alerts().await)?;
        ok(server
            .rules(Parameters(RulesParams { rule_type: None }))
            .await)?;
        ok(server
            .metadata(Parameters(MetadataParams { metric: None }))
            .await)?;
        ok(server.tsdb_status().await)?;
        ok(server.build_info().await)?;

        let requests = mock
            .received_requests()
            .await
            .ok_or_else(|| eyre!("mock server is not recording requests"))?;
        assert_eq!(
            requests.len(),
            11,
            "every tool should have issued a request"
        );
        for req in &requests {
            assert_eq!(
                req.method.as_str(),
                "GET",
                "{} used a non-GET method",
                req.url
            );
        }
        Ok(())
    }
}
