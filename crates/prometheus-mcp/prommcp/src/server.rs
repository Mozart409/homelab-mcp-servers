//! MCP server: exposes a Prometheus instance as read-only query and status tools.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::client::{PromClient, seg};

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
