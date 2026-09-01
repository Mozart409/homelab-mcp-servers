//! MCP server: exposes a Grafana Loki instance as read-only `LogQL` query and
//! label tools.

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
        self.call(&format!("/loki/api/v1/label/{}/values", seg(&label)), &q)
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
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`, so build from default and assign.
        let mut info = ServerInfo::default();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::LokiClient;
    use crate::config::Config;
    use color_eyre::eyre::{Result, eyre};
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    /// Start a mock Loki that answers *every* request — any method, any path —
    /// with `template`, and point a [`LokiServer`] at it.
    ///
    /// Matching on `any()` rather than on a method/path is deliberate: a request
    /// the code got wrong still gets served, so the assertions below can report
    /// what was actually sent instead of failing with an opaque 404.
    async fn mock_loki(template: ResponseTemplate) -> Result<(MockServer, LokiServer)> {
        let mock = MockServer::start().await;
        Mock::given(any()).respond_with(template).mount(&mock).await;

        let config = Config {
            base_url: mock.uri(),
            token: None,
            org_id: None,
            insecure: false,
            bind: "127.0.0.1:0".to_string(),
            allowed_hosts: None,
        };
        let server = LokiServer::new(LokiClient::new(&config)?);
        Ok((mock, server))
    }

    /// A minimal well-formed Loki success envelope.
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
    async fn query_defaults_limit_and_omits_unset_params() -> Result<()> {
        let (mock, server) = mock_loki(ok_body()).await?;
        ok(server
            .query(Parameters(InstantQueryParams {
                query: "up".to_string(),
                time: None,
                limit: None,
                direction: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/loki/api/v1/query");
        // `limit` is always sent, defaulting to 100; `time`/`direction` are absent.
        assert_eq!(req.url.query(), Some("query=up&limit=100"));
        Ok(())
    }

    #[tokio::test]
    async fn query_sends_time_and_direction_when_set() -> Result<()> {
        let (mock, server) = mock_loki(ok_body()).await?;
        ok(server
            .query(Parameters(InstantQueryParams {
                query: "up".to_string(),
                time: Some("42".to_string()),
                limit: Some(5),
                direction: Some("forward".to_string()),
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/loki/api/v1/query");
        assert_eq!(
            req.url.query(),
            Some("query=up&limit=5&time=42&direction=forward")
        );
        Ok(())
    }

    #[tokio::test]
    async fn query_range_defaults_limit_and_omits_unset_params() -> Result<()> {
        let (mock, server) = mock_loki(ok_body()).await?;
        ok(server
            .query_range(Parameters(RangeQueryParams {
                query: "up".to_string(),
                start: None,
                end: None,
                limit: None,
                step: None,
                direction: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/loki/api/v1/query_range");
        assert_eq!(req.url.query(), Some("query=up&limit=100"));
        Ok(())
    }

    #[tokio::test]
    async fn query_range_sends_every_optional_param_when_set() -> Result<()> {
        let (mock, server) = mock_loki(ok_body()).await?;
        ok(server
            .query_range(Parameters(RangeQueryParams {
                query: "up".to_string(),
                start: Some("1".to_string()),
                end: Some("2".to_string()),
                limit: Some(7),
                step: Some("30s".to_string()),
                direction: Some("forward".to_string()),
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/loki/api/v1/query_range");
        assert_eq!(
            req.url.query(),
            Some("query=up&limit=7&start=1&end=2&step=30s&direction=forward")
        );
        Ok(())
    }

    #[tokio::test]
    async fn labels_sends_no_query_when_window_is_unset() -> Result<()> {
        let (mock, server) = mock_loki(ok_body()).await?;
        ok(server
            .labels(Parameters(LabelsParams {
                start: None,
                end: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/loki/api/v1/labels");
        assert_eq!(req.url.query(), None);
        Ok(())
    }

    #[tokio::test]
    async fn labels_sends_the_window_when_set() -> Result<()> {
        let (mock, server) = mock_loki(ok_body()).await?;
        ok(server
            .labels(Parameters(LabelsParams {
                start: Some("1".to_string()),
                end: Some("2".to_string()),
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/loki/api/v1/labels");
        assert_eq!(req.url.query(), Some("start=1&end=2"));
        Ok(())
    }

    #[tokio::test]
    async fn label_values_percent_encodes_the_label_and_sends_the_window() -> Result<()> {
        let (mock, server) = mock_loki(ok_body()).await?;
        ok(server
            .label_values(Parameters(LabelValuesParams {
                label: "foo:bar".to_string(),
                start: Some("1".to_string()),
                end: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        // The `:` must survive as `%3A` rather than restructuring the path.
        assert_eq!(req.url.path(), "/loki/api/v1/label/foo%3Abar/values");
        assert_eq!(req.url.query(), Some("start=1"));
        Ok(())
    }

    #[tokio::test]
    async fn series_repeats_the_match_param_once_per_selector() -> Result<()> {
        let (mock, server) = mock_loki(ok_body()).await?;
        ok(server
            .series(Parameters(SeriesParams {
                selectors: vec!["up".to_string(), "down".to_string()],
                start: Some("1".to_string()),
                end: Some("2".to_string()),
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/loki/api/v1/series");
        assert_eq!(
            req.url.query(),
            Some("match%5B%5D=up&match%5B%5D=down&start=1&end=2")
        );
        Ok(())
    }

    #[tokio::test]
    async fn index_stats_omits_unset_window() -> Result<()> {
        let (mock, server) = mock_loki(ok_body()).await?;
        ok(server
            .index_stats(Parameters(IndexStatsParams {
                query: "up".to_string(),
                start: None,
                end: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/loki/api/v1/index/stats");
        assert_eq!(req.url.query(), Some("query=up"));
        Ok(())
    }

    #[tokio::test]
    async fn index_stats_sends_the_window_when_set() -> Result<()> {
        let (mock, server) = mock_loki(ok_body()).await?;
        ok(server
            .index_stats(Parameters(IndexStatsParams {
                query: "up".to_string(),
                start: Some("1".to_string()),
                end: Some("2".to_string()),
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.query(), Some("query=up&start=1&end=2"));
        Ok(())
    }

    #[tokio::test]
    async fn upstream_500_maps_to_an_error() -> Result<()> {
        let (_mock, server) = mock_loki(
            ResponseTemplate::new(500).set_body_string(r#"{"status":"error","error":"boom"}"#),
        )
        .await?;

        let res = server
            .labels(Parameters(LabelsParams {
                start: None,
                end: None,
            }))
            .await;
        assert!(res.is_err(), "a 500 must surface as ErrorData, not a panic");
        Ok(())
    }

    #[tokio::test]
    async fn malformed_json_maps_to_an_error() -> Result<()> {
        let (_mock, server) =
            mock_loki(ResponseTemplate::new(200).set_body_string("not json at all")).await?;

        let res = server
            .labels(Parameters(LabelsParams {
                start: None,
                end: None,
            }))
            .await;
        assert!(
            res.is_err(),
            "a malformed body must surface as ErrorData, not a panic"
        );
        Ok(())
    }

    /// Executable form of AGENTS.md hard rule §1: this server is read-only, so
    /// every tool must reach Loki with a `GET` and nothing else.
    #[tokio::test]
    async fn every_tool_issues_only_get_requests() -> Result<()> {
        let (mock, server) = mock_loki(ok_body()).await?;

        ok(server
            .query(Parameters(InstantQueryParams {
                query: "up".to_string(),
                time: None,
                limit: None,
                direction: None,
            }))
            .await)?;
        ok(server
            .query_range(Parameters(RangeQueryParams {
                query: "up".to_string(),
                start: None,
                end: None,
                limit: None,
                step: None,
                direction: None,
            }))
            .await)?;
        ok(server
            .labels(Parameters(LabelsParams {
                start: None,
                end: None,
            }))
            .await)?;
        ok(server
            .label_values(Parameters(LabelValuesParams {
                label: "job".to_string(),
                start: None,
                end: None,
            }))
            .await)?;
        ok(server
            .series(Parameters(SeriesParams {
                selectors: vec!["up".to_string()],
                start: None,
                end: None,
            }))
            .await)?;
        ok(server
            .index_stats(Parameters(IndexStatsParams {
                query: "up".to_string(),
                start: None,
                end: None,
            }))
            .await)?;

        let requests = mock
            .received_requests()
            .await
            .ok_or_else(|| eyre!("mock server is not recording requests"))?;
        assert_eq!(requests.len(), 6, "every tool should have issued a request");
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
