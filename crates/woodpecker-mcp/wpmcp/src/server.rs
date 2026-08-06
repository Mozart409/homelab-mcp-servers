//! MCP server: exposes a Woodpecker CI instance as read-only query and status tools.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{WpClient, seg};

/// MCP server wrapping a [`WpClient`].
#[derive(Clone)]
pub struct WpServer {
    client: WpClient,
    max_log_lines: usize,
    tool_router: ToolRouter<Self>,
}

impl WpServer {
    /// Wrap a configured client as an MCP server.
    #[must_use]
    pub fn new(client: WpClient, max_log_lines: usize) -> Self {
        Self {
            client,
            max_log_lines,
            tool_router: Self::tool_router(),
        }
    }

    /// Run a GET against the Woodpecker API and render the response as pretty
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
struct ListReposParams {
    /// Whether to list all accessible repos (default: only the user's personal repos).
    #[serde(default)]
    all: Option<bool>,
    /// Optional filter by repo name substring.
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LookupRepoParams {
    /// Repository owner.
    owner: String,
    /// Repository name.
    name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetRepoParams {
    /// Repository ID.
    repo_id: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListBranchesParams {
    /// Repository ID.
    repo_id: i64,
    /// Page number for pagination (default: 1).
    #[serde(default)]
    page: Option<i64>,
    /// Number of items per page (default: 10). Sent as `perPage` to the API.
    #[serde(default)]
    per_page: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListPullRequestsParams {
    /// Repository ID.
    repo_id: i64,
    /// Page number for pagination (default: 1).
    #[serde(default)]
    page: Option<i64>,
    /// Number of items per page (default: 10). Sent as `perPage` to the API.
    #[serde(default)]
    per_page: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListPipelinesParams {
    /// Repository ID.
    repo_id: i64,
    /// Page number for pagination (default: 1).
    #[serde(default)]
    page: Option<i64>,
    /// Number of items per page (default: 10). Sent as `perPage` to the API.
    #[serde(default)]
    per_page: Option<i64>,
    /// Filter by branch name.
    #[serde(default)]
    branch: Option<String>,
    /// Filter by event type (e.g. `push`, `pull_request`, `cron`).
    #[serde(default)]
    event: Option<String>,
    /// Filter by pipeline status (e.g., success, failure).
    #[serde(default)]
    status: Option<String>,
    /// Filter by ref (commit, tag, branch). Sent as `ref` to the API.
    #[serde(default, rename = "ref")]
    ref_: Option<String>,
    /// Return pipelines before this timestamp.
    #[serde(default)]
    before: Option<String>,
    /// Return pipelines after this timestamp.
    #[serde(default)]
    after: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetPipelineParams {
    /// Repository ID.
    repo_id: i64,
    /// Pipeline number.
    number: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PipelineConfigParams {
    /// Repository ID.
    repo_id: i64,
    /// Pipeline number.
    number: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PipelineMetadataParams {
    /// Repository ID.
    repo_id: i64,
    /// Pipeline number.
    number: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct StepLogsParams {
    /// Repository ID.
    repo_id: i64,
    /// Pipeline number.
    number: i64,
    /// Step ID.
    step_id: i64,
    /// Maximum number of log lines to return. If omitted, uses the server's
    /// configured default. If the actual log is longer than this cap, only the
    /// last N entries are returned with metadata about truncation.
    #[serde(default)]
    max_lines: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListCronsParams {
    /// Repository ID.
    repo_id: i64,
    /// Page number for pagination (default: 1).
    #[serde(default)]
    page: Option<i64>,
    /// Number of items per page (default: 10). Sent as `perPage` to the API.
    #[serde(default)]
    per_page: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListAgentsParams {
    /// Page number for pagination (default: 1).
    #[serde(default)]
    page: Option<i64>,
    /// Number of items per page (default: 10). Sent as `perPage` to the API.
    #[serde(default)]
    per_page: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListAgentTasksParams {
    /// Agent ID, as reported by `list_agents`.
    agent_id: i64,
}

/// Decode each log entry's base64 `data` field into readable text, in place.
///
/// Woodpecker models a log line's payload as Go `[]byte`, which marshals to
/// standard padded base64. Passing that through verbatim hands the caller
/// `KyBnaXQgaW5pdCA...` instead of `+ git init ...`, which makes the single most
/// useful tool here — "why did this step fail?" — unreadable.
///
/// Anything that does not decode cleanly is left exactly as received rather than
/// raising an error: one odd line should not cost the caller the whole log, and
/// if a future Woodpecker version stops encoding this field, the tool degrades to
/// passthrough instead of breaking. Invalid UTF-8 is decoded lossily for the same
/// reason — build logs carry terminal escapes and occasionally raw bytes.
fn decode_log_entries(entries: &mut [Value]) {
    for entry in entries {
        let decoded = entry
            .get("data")
            .and_then(Value::as_str)
            .and_then(|encoded| BASE64.decode(encoded).ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());

        if let (Some(text), Some(object)) = (decoded, entry.as_object_mut()) {
            object.insert("data".to_string(), Value::String(text));
        }
    }
}

/// Response wrapper for truncated log entries, keeping only the last N entries.
#[derive(Debug, Serialize)]
struct TruncatedLogs {
    truncated: bool,
    total_entries: usize,
    returned_entries: usize,
    note: String,
    entries: Vec<Value>,
}

// ---- Tools ------------------------------------------------------------------

#[tool_router]
impl WpServer {
    // `version` and `healthz` sit at the server root, NOT under the `/api` base
    // path the Swagger spec declares for everything else. Woodpecker serves the
    // web UI as a catch-all, so `/api/version` does not 404 — it quietly returns
    // the SPA's index.html with a 200, which then fails to parse as JSON. Verified
    // against Woodpecker 3.16.0: `/version` returns JSON and `/healthz` returns 204.
    #[tool(description = "Get the Woodpecker CI server version and build information.")]
    async fn version(&self) -> Result<String, ErrorData> {
        self.call("/version", &[]).await
    }

    #[tool(
        description = "Check server health. A successful call (no error) indicates the Woodpecker server is up; the endpoint returns 204 No Content, so the result is `null`."
    )]
    async fn healthz(&self) -> Result<String, ErrorData> {
        self.call("/healthz", &[]).await
    }

    #[tool(description = "Get queue information: pending, running, and waiting pipeline counts.")]
    async fn queue_info(&self) -> Result<String, ErrorData> {
        self.call("/api/queue/info", &[]).await
    }

    #[tool(description = "List repositories accessible to the user, optionally filtering by name.")]
    async fn list_repos(
        &self,
        Parameters(ListReposParams { all, name }): Parameters<ListReposParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(a) = all {
            q.push(("all", a.to_string()));
        }
        if let Some(n) = name {
            q.push(("name", n));
        }
        self.call("/api/user/repos", &q).await
    }

    #[tool(description = "Look up a repository by owner and name, returning its full details.")]
    async fn lookup_repo(
        &self,
        Parameters(LookupRepoParams { owner, name }): Parameters<LookupRepoParams>,
    ) -> Result<String, ErrorData> {
        // IMPORTANT: seg() each part independently, then join with `/`.
        // Do NOT seg() the joined string, as that would encode the separator to %2F.
        let path = format!("/api/repos/lookup/{}/{}", seg(&owner), seg(&name));
        self.call(&path, &[]).await
    }

    #[tool(description = "Get full details of a repository by ID.")]
    async fn get_repo(
        &self,
        Parameters(GetRepoParams { repo_id }): Parameters<GetRepoParams>,
    ) -> Result<String, ErrorData> {
        self.call(&format!("/api/repos/{repo_id}"), &[]).await
    }

    #[tool(
        description = "List branches in a repository. Use pagination to browse large branch lists."
    )]
    async fn list_branches(
        &self,
        Parameters(ListBranchesParams {
            repo_id,
            page,
            per_page,
        }): Parameters<ListBranchesParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(p) = page {
            q.push(("page", p.to_string()));
        }
        if let Some(pp) = per_page {
            q.push(("perPage", pp.to_string()));
        }
        self.call(&format!("/api/repos/{repo_id}/branches"), &q)
            .await
    }

    #[tool(
        description = "List pull requests in a repository. Use pagination to browse large PR lists."
    )]
    async fn list_pull_requests(
        &self,
        Parameters(ListPullRequestsParams {
            repo_id,
            page,
            per_page,
        }): Parameters<ListPullRequestsParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(p) = page {
            q.push(("page", p.to_string()));
        }
        if let Some(pp) = per_page {
            q.push(("perPage", pp.to_string()));
        }
        self.call(&format!("/api/repos/{repo_id}/pull_requests"), &q)
            .await
    }

    #[tool(
        description = "List pipelines in a repository, optionally filtered by branch, event, status, and timestamp. Use pagination for large pipeline histories."
    )]
    async fn list_pipelines(
        &self,
        Parameters(ListPipelinesParams {
            repo_id,
            page,
            per_page,
            branch,
            event,
            status,
            ref_,
            before,
            after,
        }): Parameters<ListPipelinesParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(p) = page {
            q.push(("page", p.to_string()));
        }
        if let Some(pp) = per_page {
            q.push(("perPage", pp.to_string()));
        }
        if let Some(b) = branch {
            q.push(("branch", b));
        }
        if let Some(e) = event {
            q.push(("event", e));
        }
        if let Some(s) = status {
            q.push(("status", s));
        }
        if let Some(r) = ref_ {
            q.push(("ref", r));
        }
        if let Some(bf) = before {
            q.push(("before", bf));
        }
        if let Some(af) = after {
            q.push(("after", af));
        }
        self.call(&format!("/api/repos/{repo_id}/pipelines"), &q)
            .await
    }

    // The cancel_info breakdown is in the description rather than a comment
    // because it is the caller who needs it: the object is easy to overlook, and
    // the "killed with empty cancel_info" case is invisible in this API — it was
    // confirmed by correlating a killed pipeline here against the server journal,
    // which is the only place the queue's expiry is recorded.
    #[tool(
        description = "Get detailed information about a specific pipeline, including its steps, status, and configuration. To determine WHY a pipeline stopped, read the `cancel_info` object first — it is the fastest answer: `canceled_by_user` set means a human cancelled it; `superseded_by` set means it was auto-cancelled by a newer pipeline (the repo's cancel_previous_pipeline_events setting); `canceled_by_step` set means a step cancelled it. If `status` is \"killed\" but `cancel_info` is null or empty, NOBODY cancelled it — the server's queue expired the task. That reason appears only in the Woodpecker server/agent journal (\"queue: task expired\", \"failed to extend workflow lease\"), never in this API, so stop looking for it here."
    )]
    async fn get_pipeline(
        &self,
        Parameters(GetPipelineParams { repo_id, number }): Parameters<GetPipelineParams>,
    ) -> Result<String, ErrorData> {
        self.call(&format!("/api/repos/{repo_id}/pipelines/{number}"), &[])
            .await
    }

    #[tool(description = "Get the YAML configuration used to run a specific pipeline.")]
    async fn pipeline_config(
        &self,
        Parameters(PipelineConfigParams { repo_id, number }): Parameters<PipelineConfigParams>,
    ) -> Result<String, ErrorData> {
        self.call(
            &format!("/api/repos/{repo_id}/pipelines/{number}/config"),
            &[],
        )
        .await
    }

    #[tool(
        description = "Get metadata about a pipeline, including execution time and environment details."
    )]
    async fn pipeline_metadata(
        &self,
        Parameters(PipelineMetadataParams { repo_id, number }): Parameters<PipelineMetadataParams>,
    ) -> Result<String, ErrorData> {
        self.call(
            &format!("/api/repos/{repo_id}/pipelines/{number}/metadata"),
            &[],
        )
        .await
    }

    #[tool(
        description = "Get log output from a pipeline step — use this to diagnose why a step failed. Returns an array of log entries whose `data` field holds the line's text (Woodpecker base64-encodes it on the wire; this tool decodes it). If the log exceeds max_lines, the last N entries are returned with truncation metadata."
    )]
    async fn step_logs(
        &self,
        Parameters(StepLogsParams {
            repo_id,
            number,
            step_id,
            max_lines,
        }): Parameters<StepLogsParams>,
    ) -> Result<String, ErrorData> {
        let raw = self
            .client
            .get(
                &format!("/api/repos/{repo_id}/logs/{number}/{step_id}"),
                &[],
            )
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;

        // Determine the effective cap: use max_lines param if Some, else the server default.
        let cap = max_lines.unwrap_or(self.max_log_lines);

        // If the response is an array, truncate if necessary, then decode the
        // entries actually being returned. Truncating first keeps the decode
        // proportional to what the caller sees, not to the whole log.
        let result = if let Value::Array(mut entries) = raw {
            if entries.len() > cap {
                // Keep the last `cap` entries (failure's cause is at the end).
                let total = entries.len();
                let skip = total.saturating_sub(cap);
                let mut kept_entries: Vec<Value> = entries.into_iter().skip(skip).collect();
                decode_log_entries(&mut kept_entries);

                let truncated = TruncatedLogs {
                    truncated: true,
                    total_entries: total,
                    returned_entries: kept_entries.len(),
                    note: format!(
                        "showing the last {} of {} log entries; raise max_lines or WP_MAX_LOG_LINES for more",
                        kept_entries.len(),
                        total
                    ),
                    entries: kept_entries,
                };
                serde_json::to_value(&truncated).map_err(|e| {
                    ErrorData::internal_error(
                        format!("failed to serialize truncated logs: {e}"),
                        None,
                    )
                })?
            } else {
                decode_log_entries(&mut entries);
                Value::Array(entries)
            }
        } else {
            raw
        };

        serde_json::to_string_pretty(&result).map_err(|e| {
            ErrorData::internal_error(format!("failed to serialize response: {e}"), None)
        })
    }

    #[tool(
        description = "List cron jobs configured for a repository. Use pagination to browse large cron lists."
    )]
    async fn list_crons(
        &self,
        Parameters(ListCronsParams {
            repo_id,
            page,
            per_page,
        }): Parameters<ListCronsParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(p) = page {
            q.push(("page", p.to_string()));
        }
        if let Some(pp) = per_page {
            q.push(("perPage", pp.to_string()));
        }
        self.call(&format!("/api/repos/{repo_id}/cron"), &q).await
    }

    #[tool(
        description = "List all agents registered with the Woodpecker CI server. Note: requires admin rights and may return 403 if permission is denied."
    )]
    async fn list_agents(
        &self,
        Parameters(ListAgentsParams { page, per_page }): Parameters<ListAgentsParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(p) = page {
            q.push(("page", p.to_string()));
        }
        if let Some(pp) = per_page {
            q.push(("perPage", pp.to_string()));
        }
        self.call("/api/agents", &q).await
    }

    // No pagination on this route: verified against the Woodpecker 3.16.0 Swagger
    // spec, `agent_id` is the only parameter it accepts. Sending page/perPage here
    // would be silently ignored and would mislead the caller into thinking they
    // had seen a page rather than the whole set.
    #[tool(
        description = "List the tasks an agent is currently holding. This is a LIVE view of the queue only: tasks are removed once they complete or expire, so a finished pipeline shows nothing here — use get_pipeline and step_logs for post-mortems. Its use is catching a wedge while it is happening, e.g. an agent still holding a task the server has already dropped. Each task carries its pipeline_id, repo_id, pid, name, dependencies, dep_status, labels, and created timestamp. Note: requires admin rights and may return 401 or 403 if permission is denied."
    )]
    async fn list_agent_tasks(
        &self,
        Parameters(ListAgentTasksParams { agent_id }): Parameters<ListAgentTasksParams>,
    ) -> Result<String, ErrorData> {
        self.call(&format!("/api/agents/{agent_id}/tasks"), &[])
            .await
    }

    #[tool(
        description = "Get the user's pipeline feed: recent pipelines across all their repositories."
    )]
    async fn pipeline_feed(&self) -> Result<String, ErrorData> {
        self.call("/api/user/feed", &[]).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WpServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`, so build from default and assign.
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Read-only access to a Woodpecker CI instance. Use these tools to inspect \
             repositories, pipelines, steps, logs, queue, and agents. No tool can modify CI \
             state; secrets and registries are deliberately not exposed."
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
    use crate::config::Config;
    use color_eyre::eyre::{Result, eyre};
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    /// Start a mock Woodpecker that answers *every* request — any method, any
    /// path — with `template`, and point a [`WpServer`] at it.
    ///
    /// Matching on `any()` rather than on a method/path is deliberate: a request
    /// the code got wrong still gets served, so the assertions below can report
    /// what was actually sent instead of failing with an opaque 404.
    async fn mock_wp(template: ResponseTemplate) -> Result<(MockServer, WpServer)> {
        let mock = MockServer::start().await;
        Mock::given(any()).respond_with(template).mount(&mock).await;

        let config = Config {
            base_url: mock.uri(),
            token: "test-token".to_string(),
            insecure: false,
            bind: "127.0.0.1:0".to_string(),
            allowed_hosts: None,
            max_log_lines: 500,
        };
        let server = WpServer::new(WpClient::new(&config)?, 500);
        Ok((mock, server))
    }

    /// A minimal well-formed Woodpecker success response.
    fn ok_body() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_string("{}")
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
    async fn version_hits_its_path() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server.version().await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/version");
        Ok(())
    }

    /// Every Woodpecker endpoint requires auth, so the bearer token baked into
    /// the client at construction must actually reach the wire — a missing
    /// header would turn every tool into an opaque 401.
    #[tokio::test]
    async fn requests_carry_the_bearer_token() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server.version().await)?;

        let req = only_request(&mock).await?;
        let auth = req
            .headers
            .get(reqwest::header::AUTHORIZATION)
            .ok_or_else(|| eyre!("no Authorization header was sent"))?;
        assert_eq!(auth.to_str()?, "Bearer test-token");
        Ok(())
    }

    #[tokio::test]
    async fn healthz_hits_its_path() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server.healthz().await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/healthz");
        Ok(())
    }

    #[tokio::test]
    async fn queue_info_hits_its_path() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server.queue_info().await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/queue/info");
        Ok(())
    }

    #[tokio::test]
    async fn list_repos_omits_optionals_when_none() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .list_repos(Parameters(ListReposParams {
                all: None,
                name: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/user/repos");
        assert_eq!(req.url.query(), None);
        Ok(())
    }

    #[tokio::test]
    async fn list_repos_sends_all_when_some() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .list_repos(Parameters(ListReposParams {
                all: Some(true),
                name: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.query(), Some("all=true"));
        Ok(())
    }

    #[tokio::test]
    async fn list_repos_sends_name_when_some() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .list_repos(Parameters(ListReposParams {
                all: None,
                name: Some("myrepo".to_string()),
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.query(), Some("name=myrepo"));
        Ok(())
    }

    #[tokio::test]
    async fn lookup_repo_encodes_owner_and_name_separately() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .lookup_repo(Parameters(LookupRepoParams {
                owner: "owner/part".to_string(),
                name: "name/part".to_string(),
            }))
            .await)?;

        let req = only_request(&mock).await?;
        // The owner and name are separately encoded; the `/` between them is literal.
        assert_eq!(req.url.path(), "/api/repos/lookup/owner%2Fpart/name%2Fpart");
        Ok(())
    }

    #[tokio::test]
    async fn get_repo_hits_its_path() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .get_repo(Parameters(GetRepoParams { repo_id: 42 }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/repos/42");
        Ok(())
    }

    #[tokio::test]
    async fn list_branches_omits_pagination_when_none() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .list_branches(Parameters(ListBranchesParams {
                repo_id: 1,
                page: None,
                per_page: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/repos/1/branches");
        assert_eq!(req.url.query(), None);
        Ok(())
    }

    #[tokio::test]
    async fn list_branches_renames_per_page_to_per_page() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .list_branches(Parameters(ListBranchesParams {
                repo_id: 1,
                page: Some(2),
                per_page: Some(20),
            }))
            .await)?;

        let req = only_request(&mock).await?;
        // The wire param must be `perPage`, not `per_page`.
        assert_eq!(req.url.query(), Some("page=2&perPage=20"));
        Ok(())
    }

    #[tokio::test]
    async fn list_pull_requests_omits_pagination_when_none() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .list_pull_requests(Parameters(ListPullRequestsParams {
                repo_id: 1,
                page: None,
                per_page: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/repos/1/pull_requests");
        assert_eq!(req.url.query(), None);
        Ok(())
    }

    #[tokio::test]
    async fn list_pipelines_sends_all_filters_when_set() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .list_pipelines(Parameters(ListPipelinesParams {
                repo_id: 1,
                page: Some(1),
                per_page: Some(10),
                branch: Some("main".to_string()),
                event: Some("push".to_string()),
                status: Some("success".to_string()),
                ref_: Some("abc123".to_string()),
                before: None,
                after: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        let query = req.url.query().unwrap();
        assert!(query.contains("page=1"));
        assert!(query.contains("perPage=10"));
        assert!(query.contains("branch=main"));
        assert!(query.contains("event=push"));
        assert!(query.contains("status=success"));
        // The MCP-facing field is `ref_`; it must go out as `ref` on the wire.
        assert!(query.contains("ref=abc123"));
        Ok(())
    }

    #[tokio::test]
    async fn list_pipelines_renames_ref_to_ref() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .list_pipelines(Parameters(ListPipelinesParams {
                repo_id: 1,
                page: None,
                per_page: None,
                branch: None,
                event: None,
                status: None,
                ref_: Some("myref".to_string()),
                before: None,
                after: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        // The MCP-facing field is `ref_` (Rust keyword avoidance); the wire param is `ref`.
        assert_eq!(req.url.query(), Some("ref=myref"));
        Ok(())
    }

    #[tokio::test]
    async fn get_pipeline_hits_its_path() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .get_pipeline(Parameters(GetPipelineParams {
                repo_id: 1,
                number: 42,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/repos/1/pipelines/42");
        Ok(())
    }

    #[tokio::test]
    async fn pipeline_config_hits_its_path() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .pipeline_config(Parameters(PipelineConfigParams {
                repo_id: 1,
                number: 42,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/repos/1/pipelines/42/config");
        Ok(())
    }

    #[tokio::test]
    async fn pipeline_metadata_hits_its_path() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .pipeline_metadata(Parameters(PipelineMetadataParams {
                repo_id: 1,
                number: 42,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/repos/1/pipelines/42/metadata");
        Ok(())
    }

    #[tokio::test]
    async fn step_logs_without_truncation() -> Result<()> {
        // `_mock` is unused but must stay bound: dropping it shuts the mock server down.
        // `data` is base64 on the wire, exactly as Woodpecker sends it:
        // "+ git init" and "done".
        let (_mock, server) = mock_wp(
            ResponseTemplate::new(200).set_body_string(
                r#"[{"line":1,"time":1,"type":0,"data":"KyBnaXQgaW5pdA=="},{"line":2,"time":2,"type":0,"data":"ZG9uZQ=="}]"#,
            ),
        )
        .await?;
        let result = ok(server
            .step_logs(Parameters(StepLogsParams {
                repo_id: 1,
                number: 42,
                step_id: 1,
                max_lines: Some(10),
            }))
            .await)?;

        // Under the cap the array comes back unwrapped — but decoded.
        let parsed: Value = serde_json::from_str(&result)?;
        assert!(parsed.is_array());
        let entries = parsed.as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries.first().unwrap().get("data").unwrap(), "+ git init");
        assert_eq!(entries.get(1).unwrap().get("data").unwrap(), "done");
        Ok(())
    }

    /// Woodpecker sends each log line as base64 (Go marshals `[]byte` that way).
    /// Passing it through verbatim would hand the model `KyBnaXQ...` instead of
    /// readable text, defeating the point of the tool.
    #[tokio::test]
    async fn step_logs_decodes_base64_and_survives_odd_entries() -> Result<()> {
        // A well-formed line, a value that is not valid base64, an entry with no
        // `data` key at all, and a non-object entry.
        let body = r#"[
            {"line":1,"data":"KyBjYXJnbyB0ZXN0"},
            {"line":2,"data":"!!! not base64 !!!"},
            {"line":3},
            "bare string"
        ]"#;
        let (_mock, server) = mock_wp(ResponseTemplate::new(200).set_body_string(body)).await?;
        let result = ok(server
            .step_logs(Parameters(StepLogsParams {
                repo_id: 1,
                number: 42,
                step_id: 1,
                max_lines: None,
            }))
            .await)?;

        let parsed: Value = serde_json::from_str(&result)?;
        let entries = parsed.as_array().unwrap();
        assert_eq!(entries.len(), 4);
        // Decoded.
        assert_eq!(
            entries.first().unwrap().get("data").unwrap(),
            "+ cargo test"
        );
        // Undecodable input is left exactly as received rather than erroring.
        assert_eq!(
            entries.get(1).unwrap().get("data").unwrap(),
            "!!! not base64 !!!"
        );
        // A missing `data` key stays missing; a non-object entry is untouched.
        assert!(entries.get(2).unwrap().get("data").is_none());
        assert_eq!(entries.get(3).unwrap(), "bare string");
        Ok(())
    }

    #[tokio::test]
    async fn step_logs_with_truncation() -> Result<()> {
        // Create a mock that returns 100 log entries.
        let entries: Vec<Value> = (1..=100)
            .map(|i| {
                serde_json::json!({
                    "line": i,
                    "time": i,
                    "type": 0,
                    "data": BASE64.encode(format!("log{i}"))
                })
            })
            .collect();
        let body = serde_json::to_string(&entries)?;

        // `_mock` is unused but must stay bound: dropping it shuts the mock server down.
        let (_mock, server) = mock_wp(ResponseTemplate::new(200).set_body_string(&body)).await?;
        let result = ok(server
            .step_logs(Parameters(StepLogsParams {
                repo_id: 1,
                number: 42,
                step_id: 1,
                max_lines: Some(10),
            }))
            .await)?;

        let parsed: Value = serde_json::from_str(&result)?;
        // Should be a truncation wrapper, not an array.
        assert!(parsed.is_object());
        assert_eq!(parsed.get("truncated").unwrap(), &Value::Bool(true));
        assert_eq!(
            parsed.get("total_entries").unwrap(),
            &Value::Number(100.into())
        );
        assert_eq!(
            parsed.get("returned_entries").unwrap(),
            &Value::Number(10.into())
        );
        // Should contain only the last 10 entries (lines 91–100).
        let entries_array = parsed.get("entries").unwrap().as_array().unwrap();
        assert_eq!(entries_array.len(), 10);
        assert_eq!(
            entries_array.first().unwrap().get("line").unwrap(),
            &Value::Number(91.into())
        );
        assert_eq!(
            entries_array.last().unwrap().get("line").unwrap(),
            &Value::Number(100.into())
        );
        // Truncation and decoding compose: the kept entries are decoded too.
        assert_eq!(entries_array.first().unwrap().get("data").unwrap(), "log91");
        assert_eq!(entries_array.last().unwrap().get("data").unwrap(), "log100");
        Ok(())
    }

    #[tokio::test]
    async fn step_logs_uses_server_default_when_max_lines_omitted() -> Result<()> {
        // Create 600 entries, but server default is 500.
        let entries: Vec<Value> = (1..=600)
            .map(|i| {
                serde_json::json!({
                    "line": i,
                    "time": i,
                    "type": 0,
                    "data": BASE64.encode(format!("log{i}"))
                })
            })
            .collect();
        let body = serde_json::to_string(&entries)?;

        let mock = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_string(&body))
            .mount(&mock)
            .await;

        let config = Config {
            base_url: mock.uri(),
            token: "test-token".to_string(),
            insecure: false,
            bind: "127.0.0.1:0".to_string(),
            allowed_hosts: None,
            max_log_lines: 500,
        };
        let server = WpServer::new(WpClient::new(&config)?, 500);

        let result = ok(server
            .step_logs(Parameters(StepLogsParams {
                repo_id: 1,
                number: 42,
                step_id: 1,
                max_lines: None,
            }))
            .await)?;

        let parsed: Value = serde_json::from_str(&result)?;
        assert!(parsed.is_object());
        // Should truncate to 500 (the server default).
        assert_eq!(
            parsed.get("returned_entries").unwrap(),
            &Value::Number(500.into())
        );
        Ok(())
    }

    #[tokio::test]
    async fn list_crons_omits_pagination_when_none() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .list_crons(Parameters(ListCronsParams {
                repo_id: 1,
                page: None,
                per_page: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/repos/1/cron");
        assert_eq!(req.url.query(), None);
        Ok(())
    }

    #[tokio::test]
    async fn list_agents_omits_pagination_when_none() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .list_agents(Parameters(ListAgentsParams {
                page: None,
                per_page: None,
            }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/agents");
        assert_eq!(req.url.query(), None);
        Ok(())
    }

    /// The route takes no query parameters at all, so an empty query string is
    /// part of the contract, not an incidental detail.
    #[tokio::test]
    async fn list_agent_tasks_hits_its_path() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server
            .list_agent_tasks(Parameters(ListAgentTasksParams { agent_id: 1 }))
            .await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/agents/1/tasks");
        assert_eq!(req.url.query(), None);
        Ok(())
    }

    #[tokio::test]
    async fn pipeline_feed_hits_its_path() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;
        ok(server.pipeline_feed().await)?;

        let req = only_request(&mock).await?;
        assert_eq!(req.url.path(), "/api/user/feed");
        Ok(())
    }

    #[tokio::test]
    async fn upstream_500_maps_to_an_error() -> Result<()> {
        let (_mock, server) =
            mock_wp(ResponseTemplate::new(500).set_body_string(r#"{"message":"boom"}"#)).await?;

        assert!(
            server.version().await.is_err(),
            "a 500 must surface as ErrorData, not a panic"
        );
        Ok(())
    }

    #[tokio::test]
    async fn malformed_json_maps_to_an_error() -> Result<()> {
        let (_mock, server) =
            mock_wp(ResponseTemplate::new(200).set_body_string("not json at all")).await?;

        assert!(
            server.version().await.is_err(),
            "a malformed body must surface as ErrorData, not a panic"
        );
        Ok(())
    }

    #[tokio::test]
    async fn error_with_an_empty_body_has_no_dangling_separator() -> Result<()> {
        // Woodpecker answers a 404 with no body at all. Appending an empty
        // message left `... returned 404 Not Found: ` with nothing after the
        // colon, which reads like the error itself was truncated.
        let (_mock, server) = mock_wp(ResponseTemplate::new(404).set_body_string("")).await?;

        let err = server
            .version()
            .await
            .expect_err("a 404 must surface as ErrorData");
        let message = format!("{err:?}");
        assert!(
            message.contains("404"),
            "the status must survive into the message, got: {message}"
        );
        assert!(
            !message.contains("Not Found: "),
            "no separator without a message after it, got: {message}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn empty_200_body_yields_null() -> Result<()> {
        let (_mock, server) = mock_wp(ResponseTemplate::new(200).set_body_string("")).await?;

        let result = ok(server.healthz().await)?;
        let parsed: Value = serde_json::from_str(&result)?;
        assert_eq!(parsed, Value::Null);
        Ok(())
    }

    /// Invoke all 17 tools once, with minimal arguments.
    ///
    /// Split out of the test below purely so neither function trips
    /// `clippy::too_many_lines` — enumerating every tool is inherently long, and
    /// the enumeration is the point: a tool added without a line here would slip
    /// past the read-only assertion.
    async fn invoke_every_tool(server: &WpServer) -> Result<()> {
        ok(server.version().await)?;
        ok(server.healthz().await)?;
        ok(server.queue_info().await)?;
        ok(server
            .list_repos(Parameters(ListReposParams {
                all: None,
                name: None,
            }))
            .await)?;
        ok(server
            .lookup_repo(Parameters(LookupRepoParams {
                owner: "owner".to_string(),
                name: "repo".to_string(),
            }))
            .await)?;
        ok(server
            .get_repo(Parameters(GetRepoParams { repo_id: 1 }))
            .await)?;
        ok(server
            .list_branches(Parameters(ListBranchesParams {
                repo_id: 1,
                page: None,
                per_page: None,
            }))
            .await)?;
        ok(server
            .list_pull_requests(Parameters(ListPullRequestsParams {
                repo_id: 1,
                page: None,
                per_page: None,
            }))
            .await)?;
        ok(server
            .list_pipelines(Parameters(ListPipelinesParams {
                repo_id: 1,
                page: None,
                per_page: None,
                branch: None,
                event: None,
                status: None,
                ref_: None,
                before: None,
                after: None,
            }))
            .await)?;
        ok(server
            .get_pipeline(Parameters(GetPipelineParams {
                repo_id: 1,
                number: 1,
            }))
            .await)?;
        ok(server
            .pipeline_config(Parameters(PipelineConfigParams {
                repo_id: 1,
                number: 1,
            }))
            .await)?;
        ok(server
            .pipeline_metadata(Parameters(PipelineMetadataParams {
                repo_id: 1,
                number: 1,
            }))
            .await)?;
        ok(server
            .step_logs(Parameters(StepLogsParams {
                repo_id: 1,
                number: 1,
                step_id: 1,
                max_lines: None,
            }))
            .await)?;
        ok(server
            .list_crons(Parameters(ListCronsParams {
                repo_id: 1,
                page: None,
                per_page: None,
            }))
            .await)?;
        ok(server
            .list_agents(Parameters(ListAgentsParams {
                page: None,
                per_page: None,
            }))
            .await)?;
        ok(server
            .list_agent_tasks(Parameters(ListAgentTasksParams { agent_id: 1 }))
            .await)?;
        ok(server.pipeline_feed().await)?;
        Ok(())
    }

    /// Executable form of AGENTS.md hard rule §1: this server is read-only, so
    /// every tool must reach Woodpecker with a `GET` and nothing else.
    #[tokio::test]
    async fn every_tool_issues_only_get_requests() -> Result<()> {
        let (mock, server) = mock_wp(ok_body()).await?;

        invoke_every_tool(&server).await?;

        let requests = mock
            .received_requests()
            .await
            .ok_or_else(|| eyre!("mock server is not recording requests"))?;
        assert_eq!(
            requests.len(),
            17,
            "all 17 tools should have issued a request"
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
