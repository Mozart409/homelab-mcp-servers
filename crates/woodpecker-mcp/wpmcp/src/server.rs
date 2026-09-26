//! MCP server: exposes a Woodpecker CI instance as read-only query and status tools.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use mcp_common::NoArguments;
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
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{WpClient, seg};

/// MCP server wrapping a [`WpClient`].
#[derive(Clone)]
pub struct WpServer {
    client: WpClient,
    max_log_lines: usize,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl WpServer {
    /// Wrap a configured client as an MCP server.
    #[must_use]
    pub fn new(client: WpClient, max_log_lines: usize) -> Self {
        Self {
            client,
            max_log_lines,
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
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
#[serde(deny_unknown_fields)]
struct ListReposParams {
    /// Whether to list all accessible repos (default: only the user's personal repos).
    #[serde(default)]
    all: Option<bool>,
    /// Optional filter by repo name substring.
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LookupRepoParams {
    /// Repository owner.
    owner: String,
    /// Repository name.
    name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetRepoParams {
    /// Repository ID.
    repo_id: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
struct GetPipelineParams {
    /// Repository ID.
    repo_id: i64,
    /// Pipeline number.
    number: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PipelineConfigParams {
    /// Repository ID.
    repo_id: i64,
    /// Pipeline number.
    number: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PipelineMetadataParams {
    /// Repository ID.
    repo_id: i64,
    /// Pipeline number.
    number: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct StepLogsParams {
    /// Repository ID.
    repo_id: i64,
    /// Pipeline number.
    number: i64,
    /// Step ID.
    step_id: i64,
    /// Maximum number of log lines to return. If omitted or `0`, uses the
    /// server's configured default (`0` means "default", as it does for
    /// `WP_MAX_LOG_LINES`). If the actual log is longer than this cap, only the
    /// last N entries are returned with metadata about truncation.
    #[serde(default)]
    max_lines: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
struct ListAgentsParams {
    /// Page number for pagination (default: 1).
    #[serde(default)]
    page: Option<i64>,
    /// Number of items per page (default: 10). Sent as `perPage` to the API.
    #[serde(default)]
    per_page: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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

// ---- Prompt arguments -------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PipelinePostmortemArgs {
    /// Repository slug in the form `owner/name`.
    repo: String,
    /// Pipeline number to investigate. Omit to investigate the most recent failed pipeline.
    ///
    /// A string, not a number: MCP prompt arguments are always strings on the
    /// wire (`{[name]: string}`), so a numeric type here would reject every
    /// spec-compliant client's `"212"`.
    #[serde(default)]
    pipeline: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CiHealthTriageArgs {
    /// Optional repository slug to focus the triage (e.g., `owner/name`).
    /// Omit to scan all repositories in the user's feed.
    #[serde(default)]
    repo: Option<String>,
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
    async fn version(&self, _: Parameters<NoArguments>) -> Result<String, ErrorData> {
        self.call("/version", &[]).await
    }

    #[tool(
        description = "Check server health. A successful call (no error) indicates the Woodpecker server is up; the endpoint returns 204 No Content, so the result is `null`."
    )]
    async fn healthz(&self, _: Parameters<NoArguments>) -> Result<String, ErrorData> {
        self.call("/healthz", &[]).await
    }

    #[tool(description = "Get queue information: pending, running, and waiting pipeline counts.")]
    async fn queue_info(&self, _: Parameters<NoArguments>) -> Result<String, ErrorData> {
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
        let path = format!("/api/repos/lookup/{}/{}", seg(&owner)?, seg(&name)?);
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

        // The effective cap: `max_lines` when given and non-zero, else the
        // server default. `0` is "default", never "no lines": a cap of zero
        // would return an empty log that reads as "this step printed nothing".
        let cap = max_lines.filter(|&n| n > 0).unwrap_or(self.max_log_lines);

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
    async fn pipeline_feed(&self, _: Parameters<NoArguments>) -> Result<String, ErrorData> {
        self.call("/api/user/feed", &[]).await
    }
}

// ---- Prompts ----------------------------------------------------------------

/// Common Woodpecker CI diagnostic workflows, encoded as prompts.
///
/// These live in their own inherent `impl` block so `#[prompt_router]` and
/// `#[tool_router]` each own one block outright. Both generate an associated
/// router constructor (`Self::prompt_router()` / `Self::tool_router()`), and
/// keeping them separate avoids asking either macro to walk attributes it does
/// not recognise.
#[prompt_router]
impl WpServer {
    /// Diagnose why a pipeline failed, ruling out mundane explanations in one sweep.
    ///
    /// This prompt walks through the `cancel_info` structure and agent state to
    /// separate human intervention from task expiry — the most common failure mode
    /// that the web UI conflates with cancellation. See the README's post-mortems
    /// section for the reasoning.
    #[prompt(
        name = "pipeline_postmortem",
        description = "Investigate why a Woodpecker pipeline stopped: distinguish human cancellation from queue expiry, read step failures, and check agent health."
    )]
    async fn pipeline_postmortem(
        &self,
        params: Parameters<PipelinePostmortemArgs>,
    ) -> Vec<PromptMessage> {
        let PipelinePostmortemArgs { repo, pipeline } = params.0;

        let pipeline_resolution = match pipeline.as_deref().map(str::trim).filter(|p| !p.is_empty())
        {
            Some(num) => format!("Call `get_pipeline` with repo_id and pipeline number {num}."),
            None => "Call `list_pipelines` with repo_id and `status: failure` to find the most \
                     recent failed pipeline, then call `get_pipeline` on it."
                .to_string(),
        };

        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Help me investigate why a Woodpecker pipeline in repo `{repo}` failed.\n\n\
                 Work in this order:\n\n\
                 1. **Resolve the pipeline.** Call `lookup_repo` with `{repo}` to get the repo_id. Then:\n\
                    {pipeline_resolution}\n\n\
                 2. **Read the `cancel_info` object in the response.** It is the fastest answer to why \
                    the pipeline stopped:\n\
                    - If `canceled_by_user` is set: a human cancelled it — case closed.\n\
                    - If `superseded_by` is set: auto-cancelled by a newer pipeline (repo's `cancel_previous_pipeline_events` setting).\n\
                    - If `canceled_by_step` is set: a step cancelled the pipeline.\n\
                    - If `status` is `\"killed\"` but `cancel_info` is null or empty: **Nobody cancelled it — \
                      the server's queue expired the task.** This is the signature: a killed pipeline with \
                      no error in the logs and nobody to blame. Queue expiry appears only in the \
                      Woodpecker server/agent journal (\"queue: task expired\", \"failed to extend workflow \
                      lease\"), never in this API.\n\n\
                 3. **Call `step_logs` for the failed step** to see the last part of the output. If the \
                    log just stops with no error at the end, queue expiry is confirmed (the agent lost \
                    its lease and never got to write an error).\n\n\
                 4. **Check agent liveness** via `list_agents`. Find the agent that was running the \
                    pipeline (from the pipeline's metadata if available, or guess from running workloads). \
                    Look at `last_contact` and `last_work`:\n\
                    - Both timestamps quiet and recent (within the last few minutes): agent is healthy, \
                      just idle.\n\
                    - `last_contact` recent but `last_work` frozen well *before* the pipeline died: agent \
                      is *wedged* (still checking in, but not finishing work).\n\n\
                 5. **Summarise what you found.** State the reason clearly:\n\
                    - For cancellation: WHO cancelled and WHY (user, auto-supersede, or step action).\n\
                    - For queue expiry: the pipeline's start/stop timestamps from `get_pipeline`, and a \
                      note that the real cause lives only in `journalctl` on the Woodpecker server/agent \
                      (search that window for \"expired\", \"lease\", \"database is locked\", or \"pull queue item\").\n\
                    - For a wedged agent: agent ID, when it last checked in and last completed work, and \
                      the implication (agent is stuck, likely needs a restart).\n\
                 Do not speculate beyond what these tools show. The server cannot tell you why the \
                 scheduler killed something — that evidence exists only in the Woodpecker journal."
            ),
        )]
    }

    /// Triage overall CI health in one pass.
    ///
    /// This prompt uses the broadest tools to scan for widespread degradation
    /// versus individual pipeline failures.
    #[prompt(
        name = "ci_health_triage",
        description = "Health check: is the Woodpecker CI system itself degraded, or are specific pipelines just failing?"
    )]
    async fn ci_health_triage(&self, params: Parameters<CiHealthTriageArgs>) -> Vec<PromptMessage> {
        let CiHealthTriageArgs { repo } = params.0;

        let repo_filter = repo
            .as_ref()
            .map(|r| format!(" in repo `{r}`"))
            .unwrap_or_default();

        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Give me a health check on this Woodpecker CI system{repo_filter}.\n\n\
                 Work in this order:\n\n\
                 1. **Server liveness.** Call `version` and `healthz` to confirm the CI server itself \
                    is running. If either fails, the system is down — stop here.\n\n\
                 2. **Queue depth.** Call `queue_info` to see how many pipelines are pending, running, \
                    and waiting. An unusually large backlog might indicate the system is overwhelmed. \
                    Normal depth depends on your setup; note the numbers.\n\n\
                 3. **Agent health.** Call `list_agents` (requires admin rights; if you get a 401/403, \
                    skip this and report it). For each agent, check `last_contact` and `last_work`:\n\
                    - Recent timestamps: agent is healthy.\n\
                    - `last_contact` recent but `last_work` frozen for hours: agent is *wedged* and \
                      should be restarted.\n\
                    - No recent contact at all: agent is offline or unreachable.\n\n\
                 4. **Recent failures across the system.** Call `pipeline_feed` to get the user's cross-repo \
                    pipeline feed. Scan for patterns:\n\
                    - Same step failing in many repos: likely a shared infrastructure problem (test runner \
                      down, dependency repo offline, etc.).\n\
                    - Failures scattered across different repos and steps: likely individual issues.\n\
                    - A recent failure surge compared to earlier in the feed: likely a deployment or \
                      config change.\n\n\
                 5. **Report the verdict.** Is the CI system itself degraded (server slow, agents \
                    offline, queue blocked) or are specific pipelines just failing? If specific, which \
                    repos or steps? Distinguish:\n\
                    - **System degradation**: server down, all/most agents wedged, queue unable to drain, \
                      widespread failures. → Escalate to infrastructure.\n\
                    - **Isolated failures**: specific pipelines failing on their own merits in otherwise \
                      healthy repos. → Route to teams running those pipelines."
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
impl ServerHandler for WpServer {
    fn get_info(&self) -> ServerConfig {
        // `ServerConfig` is `#[non_exhaustive]`, so build from default and assign.
        let mut info = ServerConfig::default();
        info.instructions = Some(
            "Read-only access to a Woodpecker CI instance. Use these tools to inspect \
             repositories, pipelines, steps, logs, queue, and agents. No tool can modify CI \
             state; secrets and registries are deliberately not exposed."
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
    /// The README carries the post-mortems reasoning and API quirks that no tool
    /// return value contains, so exposing it as a resource lets a client understand
    /// the limitations without spending a tool call on it.
    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let uri = mcp_common::doc_resource_uri(env!("CARGO_PKG_NAME"));
        Ok(ListResourcesResult::with_all_items(vec![
            mcp_common::doc_resource(
                &uri,
                "Woodpecker MCP operator guide",
                "README for woodpecker-mcp: post-mortem reasoning, API authentication quirks, and server/agent diagnostics.",
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
