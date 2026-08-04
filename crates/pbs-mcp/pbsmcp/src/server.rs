//! MCP server: exposes PBS backup-status data as read-only tools.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::client::{PbsClient, seg};

/// MCP server wrapping a [`PbsClient`].
#[derive(Clone)]
pub struct PbsServer {
    client: PbsClient,
    tool_router: ToolRouter<Self>,
}

impl PbsServer {
    /// Wrap a configured client as an MCP server.
    #[must_use]
    pub fn new(client: PbsClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
        }
    }

    /// Run a GET against the PBS API and render the `data` payload as pretty JSON,
    /// mapping any error into an MCP error.
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
struct StoreParams {
    /// Datastore name.
    store: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GroupsParams {
    /// Datastore name.
    store: String,
    /// Namespace to list within (default: the root namespace).
    #[serde(default)]
    namespace: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SnapshotParams {
    /// Datastore name.
    store: String,
    /// Namespace to list within (default: the root namespace).
    #[serde(default)]
    namespace: Option<String>,
    /// Filter by backup type: "vm", "ct", or "host".
    #[serde(default)]
    backup_type: Option<String>,
    /// Filter by backup ID (the group ID, e.g. a VMID or hostname).
    #[serde(default)]
    backup_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TasksParams {
    /// Maximum number of tasks to return (default: 50).
    #[serde(default)]
    limit: Option<u64>,
    /// Only return tasks that finished with an error.
    #[serde(default)]
    errors_only: Option<bool>,
    /// Only return currently running tasks.
    #[serde(default)]
    running: Option<bool>,
    /// Only return tasks started at or after this UNIX epoch (seconds).
    #[serde(default)]
    since: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TaskParams {
    /// Task UPID (unique process identifier), as returned by `list_tasks`.
    upid: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TaskLogParams {
    /// Task UPID, as returned by `list_tasks`.
    upid: String,
    /// Line offset to start at (PBS `start` parameter; `0` = the beginning).
    /// Omit to start at the beginning.
    #[serde(default)]
    start: Option<u64>,
    /// Maximum number of log lines to return (default: 100).
    #[serde(default)]
    limit: Option<u64>,
    /// Return the LAST N lines instead of reading from the beginning; use this
    /// for long-running tasks. Mutually exclusive with `start`.
    #[serde(default)]
    tail: Option<u64>,
}

// ---- Tool helpers -----------------------------------------------------------

/// Compute the `start` offset that makes a log fetch return the last `tail`
/// lines of a log known to be `total` lines long.
///
/// Saturates at 0 when `tail >= total` — fetch the whole log from the
/// beginning rather than underflowing the offset.
fn tail_start(total: u64, tail: u64) -> u64 {
    total.saturating_sub(tail)
}

/// Extract the numeric `total` field from a PBS task-log response envelope,
/// erroring clearly when it is absent or not a non-negative integer.
fn envelope_total(envelope: &serde_json::Value) -> Result<u64, ErrorData> {
    envelope
        .get("total")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            ErrorData::internal_error(
                "PBS task-log response envelope missing numeric `total` field",
                None,
            )
        })
}

// ---- Tools ------------------------------------------------------------------

#[tool_router]
impl PbsServer {
    #[tool(description = "List all configured datastores with comments.")]
    async fn list_datastores(&self) -> Result<String, ErrorData> {
        self.call("/admin/datastore", &[]).await
    }

    #[tool(
        description = "Get a datastore's status: total/used/available bytes, deduplication factor, garbage-collection status, and estimated full date."
    )]
    async fn datastore_status(
        &self,
        Parameters(StoreParams { store }): Parameters<StoreParams>,
    ) -> Result<String, ErrorData> {
        self.call(&format!("/admin/datastore/{}/status", seg(&store)), &[])
            .await
    }

    #[tool(
        description = "List backup groups in a datastore, with last-backup time and backup counts."
    )]
    async fn list_groups(
        &self,
        Parameters(GroupsParams { store, namespace }): Parameters<GroupsParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(ns) = namespace {
            q.push(("ns", ns));
        }
        self.call(&format!("/admin/datastore/{}/groups", seg(&store)), &q)
            .await
    }

    #[tool(
        description = "List backup snapshots in a datastore, including backup time, size, owner, and verification state. Optionally filter by namespace, backup type, or backup ID."
    )]
    async fn list_snapshots(
        &self,
        Parameters(SnapshotParams {
            store,
            namespace,
            backup_type,
            backup_id,
        }): Parameters<SnapshotParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        if let Some(ns) = namespace {
            q.push(("ns", ns));
        }
        if let Some(t) = backup_type {
            q.push(("backup-type", t));
        }
        if let Some(id) = backup_id {
            q.push(("backup-id", id));
        }
        self.call(&format!("/admin/datastore/{}/snapshots", seg(&store)), &q)
            .await
    }

    #[tool(
        description = "List recent tasks (backups, verifies, prunes, syncs, GC) with their status. Optionally limit count, filter to errors only, running only, or since a given epoch."
    )]
    async fn list_tasks(
        &self,
        Parameters(TasksParams {
            limit,
            errors_only,
            running,
            since,
        }): Parameters<TasksParams>,
    ) -> Result<String, ErrorData> {
        let node = self.client.node.clone();
        let mut q = vec![("limit", limit.unwrap_or(50).to_string())];
        if errors_only.unwrap_or(false) {
            q.push(("errors", "1".to_string()));
        }
        if running.unwrap_or(false) {
            q.push(("running", "1".to_string()));
        }
        if let Some(s) = since {
            q.push(("since", s.to_string()));
        }
        self.call(&format!("/nodes/{}/tasks", seg(&node)), &q).await
    }

    #[tool(
        description = "Get the status of a single task by its UPID (running/stopped, exit status)."
    )]
    async fn task_status(
        &self,
        Parameters(TaskParams { upid }): Parameters<TaskParams>,
    ) -> Result<String, ErrorData> {
        let node = self.client.node.clone();
        self.call(
            &format!("/nodes/{}/tasks/{}/status", seg(&node), seg(&upid)),
            &[],
        )
        .await
    }

    #[tool(
        description = "Read the log output of a task by its UPID. Returns lines from the start of the log by default; use `start` and `limit` together to page through it, or set `tail` to fetch the last N lines (the right choice for long-running tasks, where the interesting output is at the end). The response includes the log's `total` line count so you can page."
    )]
    async fn task_log(
        &self,
        Parameters(TaskLogParams {
            upid,
            start,
            limit,
            tail,
        }): Parameters<TaskLogParams>,
    ) -> Result<String, ErrorData> {
        if tail.is_some() && start.is_some() {
            return Err(ErrorData::invalid_params(
                "`tail` and `start` are mutually exclusive: omit `start` to fetch the last N lines",
                None,
            ));
        }

        let node = self.client.node.clone();
        let path = format!("/nodes/{}/tasks/{}/log", seg(&node), seg(&upid));

        // Resolve the (start, limit) window to fetch. Tailing first learns the
        // total line count from a cheap one-line probe so the final fetch can
        // start exactly `tail` lines before the end.
        let (start, limit) = if let Some(n) = tail {
            let probe = self
                .client
                .get_envelope(
                    &path,
                    &[("start", "0".to_string()), ("limit", "1".to_string())],
                )
                .await
                .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
            (tail_start(envelope_total(&probe)?, n), n)
        } else {
            (start.unwrap_or(0), limit.unwrap_or(100))
        };

        let envelope = self
            .client
            .get_envelope(
                &path,
                &[("start", start.to_string()), ("limit", limit.to_string())],
            )
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;

        // Take `total` from the final fetch — for a running task it may have
        // grown since the probe, so it is the freshest value available.
        let total = envelope_total(&envelope)?;
        let lines = envelope.get("data").cloned().unwrap_or_default();
        let out = serde_json::json!({ "total": total, "start": start, "lines": lines });
        serde_json::to_string_pretty(&out).map_err(|e| {
            ErrorData::internal_error(format!("failed to serialize response: {e}"), None)
        })
    }

    #[tool(
        description = "List configured garbage-collection jobs and their last run status across datastores."
    )]
    async fn gc_status(&self) -> Result<String, ErrorData> {
        self.call("/admin/gc", &[]).await
    }

    #[tool(
        description = "Get overall node status: CPU, memory, swap, root filesystem usage, and uptime."
    )]
    async fn node_status(&self) -> Result<String, ErrorData> {
        let node = self.client.node.clone();
        self.call(&format!("/nodes/{}/status", seg(&node)), &[])
            .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PbsServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`, so build from default and assign.
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Read-only access to a Proxmox Backup Server. Use these tools to inspect \
             datastores, backup snapshots, and task history to check backup status."
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
    use super::{
        GroupsParams, Parameters, PbsServer, SnapshotParams, StoreParams, TaskLogParams,
        TaskParams, TasksParams, envelope_total, tail_start,
    };
    use crate::client::PbsClient;
    use crate::config::Config;
    use serde_json::json;
    use wiremock::matchers::{any, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    const API_KEY: &str = "mcp@pbs!homelab:2f9c-secret";

    // ---- pure helpers -------------------------------------------------------

    #[test]
    fn tail_start_when_tail_less_than_total() {
        assert_eq!(tail_start(1000, 12), 988);
    }

    #[test]
    fn tail_start_when_tail_greater_than_total_saturates() {
        // Must not underflow: fetch the whole log from the beginning.
        assert_eq!(tail_start(5, 100), 0);
    }

    #[test]
    fn tail_start_when_tail_equals_total() {
        assert_eq!(tail_start(100, 100), 0);
    }

    #[test]
    fn envelope_total_reads_the_total_field() {
        let env = json!({ "data": ["line one", "line two"], "total": 2 });
        assert_eq!(envelope_total(&env).unwrap(), 2);
    }

    #[test]
    fn envelope_total_accepts_zero_and_an_empty_data_array() {
        // A task that has not written any log lines yet: valid, total is 0.
        let env = json!({ "data": [], "total": 0 });
        assert_eq!(envelope_total(&env).unwrap(), 0);
    }

    #[test]
    fn envelope_total_errors_instead_of_panicking() {
        // Every failure mode must surface as `Err(ErrorData)` — these are
        // long-running daemons, a panic would take out unrelated in-flight
        // requests (AGENTS.md hard rule §7).
        let bad = [
            // Missing key entirely.
            json!({ "data": [] }),
            // Present but the wrong JSON type.
            json!({ "data": [], "total": "12" }),
            json!({ "data": [], "total": null }),
            json!({ "data": [], "total": 12.5 }),
            // Negative: `as_u64` refuses it rather than wrapping.
            json!({ "data": [], "total": -1 }),
            // Envelope is not an object at all.
            json!([]),
            json!("nope"),
        ];

        for env in &bad {
            let err = envelope_total(env).unwrap_err();
            assert!(
                err.message.contains("total"),
                "error for {env} should mention the missing field, got: {}",
                err.message
            );
        }
    }

    // ---- wiremock-backed tool tests -----------------------------------------

    /// Build a server pointed at a mock upstream.
    ///
    /// Returns `Result` rather than unwrapping: the clippy test exemption keys
    /// off the enclosing `#[test]` fn, so a free helper is still subject to the
    /// workspace `unwrap_used`/`expect_used` denials.
    fn server_for(base_url: &str) -> color_eyre::eyre::Result<PbsServer> {
        let config = Config {
            base_url: base_url.to_string(),
            api_key: API_KEY.to_string(),
            node: "pbs01".to_string(),
            insecure: false,
            bind: "127.0.0.1:8080".to_string(),
            allowed_hosts: None,
        };
        Ok(PbsServer::new(PbsClient::new(&config)?))
    }

    /// An upstream that answers anything with an empty PBS envelope.
    fn ok_envelope() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({ "data": [], "total": 0 }))
    }

    /// Decode a recorded request's query string into sorted `(key, value)` pairs.
    fn query_pairs(req: &Request) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = req
            .url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        pairs.sort();
        pairs
    }

    #[tokio::test]
    async fn list_datastores_requests_the_admin_datastore_path() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api2/json/admin/datastore"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
            .expect(1)
            .mount(&mock)
            .await;

        let out = server_for(&mock.uri())
            .unwrap()
            .list_datastores()
            .await
            .unwrap();
        assert_eq!(out, "[]");

        let reqs = mock.received_requests().await.unwrap();
        let [req] = reqs.as_slice() else {
            panic!("expected exactly 1 request, got {}", reqs.len())
        };
        assert_eq!(req.url.path(), "/api2/json/admin/datastore");
        // No optional params: no query string at all, not an empty `?`.
        assert_eq!(req.url.query(), None);
    }

    #[tokio::test]
    async fn authorization_header_uses_the_pbsapitoken_scheme() {
        let mock = MockServer::start().await;
        Mock::given(any())
            .respond_with(ok_envelope())
            .mount(&mock)
            .await;

        server_for(&mock.uri())
            .unwrap()
            .list_datastores()
            .await
            .unwrap();

        let reqs = mock.received_requests().await.unwrap();
        let auth = reqs.first().unwrap().headers.get("authorization").unwrap();
        // Pin the exact wire format: PBS rejects `Bearer`/`PVEAPIToken` and any
        // stray space around `=`. Easy to break silently, so assert byte-exact.
        assert_eq!(auth.to_str().unwrap(), format!("PBSAPIToken={API_KEY}"));
    }

    #[tokio::test]
    async fn list_snapshots_only_sends_provided_optional_params() {
        let mock = MockServer::start().await;
        Mock::given(any())
            .respond_with(ok_envelope())
            .mount(&mock)
            .await;
        let srv = server_for(&mock.uri()).unwrap();

        // Nothing optional supplied -> bare path, no query string.
        srv.list_snapshots(Parameters(SnapshotParams {
            store: "r2-store".to_string(),
            namespace: None,
            backup_type: None,
            backup_id: None,
        }))
        .await
        .unwrap();

        // All optionals supplied -> each appears under its PBS parameter name.
        srv.list_snapshots(Parameters(SnapshotParams {
            store: "r2-store".to_string(),
            namespace: Some("tenants/a".to_string()),
            backup_type: Some("ct".to_string()),
            backup_id: Some("104".to_string()),
        }))
        .await
        .unwrap();

        let reqs = mock.received_requests().await.unwrap();
        let [bare, filtered] = reqs.as_slice() else {
            panic!("expected exactly 2 requests, got {}", reqs.len())
        };

        // The store name is percent-encoded into the path (`-` -> `%2D`).
        assert_eq!(
            bare.url.path(),
            "/api2/json/admin/datastore/r2%2Dstore/snapshots"
        );
        assert_eq!(bare.url.query(), None);

        assert_eq!(
            query_pairs(filtered),
            vec![
                ("backup-id".to_string(), "104".to_string()),
                ("backup-type".to_string(), "ct".to_string()),
                ("ns".to_string(), "tenants/a".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn list_tasks_defaults_limit_and_omits_unset_filters() {
        let mock = MockServer::start().await;
        Mock::given(any())
            .respond_with(ok_envelope())
            .mount(&mock)
            .await;
        let srv = server_for(&mock.uri()).unwrap();

        // Nothing supplied -> only the default `limit=50`; the boolean filters
        // are absent rather than sent as `0`.
        srv.list_tasks(Parameters(TasksParams {
            limit: None,
            errors_only: None,
            running: None,
            since: None,
        }))
        .await
        .unwrap();

        // Explicit `false` is still an omission, not `errors=0`.
        srv.list_tasks(Parameters(TasksParams {
            limit: Some(5),
            errors_only: Some(false),
            running: Some(false),
            since: None,
        }))
        .await
        .unwrap();

        // All filters on.
        srv.list_tasks(Parameters(TasksParams {
            limit: Some(5),
            errors_only: Some(true),
            running: Some(true),
            since: Some(1_700_000_000),
        }))
        .await
        .unwrap();

        let reqs = mock.received_requests().await.unwrap();
        let [defaults, explicit_false, all_on] = reqs.as_slice() else {
            panic!("expected exactly 3 requests, got {}", reqs.len())
        };
        assert_eq!(defaults.url.path(), "/api2/json/nodes/pbs01/tasks");

        assert_eq!(
            query_pairs(defaults),
            vec![("limit".to_string(), "50".to_string())]
        );
        assert_eq!(
            query_pairs(explicit_false),
            vec![("limit".to_string(), "5".to_string())]
        );
        assert_eq!(
            query_pairs(all_on),
            vec![
                ("errors".to_string(), "1".to_string()),
                ("limit".to_string(), "5".to_string()),
                ("running".to_string(), "1".to_string()),
                ("since".to_string(), "1700000000".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn upstream_401_surfaces_as_an_error_not_a_panic() {
        let mock = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(401).set_body_string("authentication failure"))
            .mount(&mock)
            .await;

        let err = server_for(&mock.uri())
            .unwrap()
            .list_datastores()
            .await
            .unwrap_err();
        assert!(
            err.message.contains("401"),
            "status should be surfaced, got: {}",
            err.message
        );
        assert!(err.message.contains("authentication failure"));
    }

    #[tokio::test]
    async fn upstream_500_surfaces_as_an_error_not_a_panic() {
        let mock = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&mock)
            .await;

        let err = server_for(&mock.uri())
            .unwrap()
            .node_status()
            .await
            .unwrap_err();
        assert!(
            err.message.contains("500"),
            "status should be surfaced, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn task_log_rejects_tail_and_start_together() {
        let mock = MockServer::start().await;
        Mock::given(any())
            .respond_with(ok_envelope())
            .mount(&mock)
            .await;

        let err = server_for(&mock.uri())
            .unwrap()
            .task_log(Parameters(TaskLogParams {
                upid: "UPID:pbs01:0000A1B2:00000000:66000000:backup:store:root@pam:".to_string(),
                start: Some(10),
                limit: None,
                tail: Some(10),
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("mutually exclusive"));

        // The guard must reject before touching the upstream.
        assert!(mock.received_requests().await.unwrap().is_empty());
    }

    /// AGENTS.md hard rule §1, made executable: this server is read-only, so a
    /// representative sweep of every tool must produce nothing but bodyless
    /// `GET`s. The matcher accepts ANY method so a regression shows up as a
    /// failed assertion here rather than as an unmatched-request 404.
    #[tokio::test]
    async fn every_tool_issues_only_get_requests() {
        let mock = MockServer::start().await;
        Mock::given(any())
            .respond_with(ok_envelope())
            .mount(&mock)
            .await;
        let srv = server_for(&mock.uri()).unwrap();
        let upid = "UPID:pbs01:0000A1B2:00000000:66000000:backup:store:root@pam:".to_string();

        srv.list_datastores().await.unwrap();
        srv.datastore_status(Parameters(StoreParams {
            store: "r2-store".to_string(),
        }))
        .await
        .unwrap();
        srv.list_groups(Parameters(GroupsParams {
            store: "r2-store".to_string(),
            namespace: Some("tenants/a".to_string()),
        }))
        .await
        .unwrap();
        srv.list_snapshots(Parameters(SnapshotParams {
            store: "r2-store".to_string(),
            namespace: None,
            backup_type: Some("ct".to_string()),
            backup_id: None,
        }))
        .await
        .unwrap();
        srv.list_tasks(Parameters(TasksParams {
            limit: Some(5),
            errors_only: Some(true),
            running: Some(true),
            since: Some(1),
        }))
        .await
        .unwrap();
        srv.task_status(Parameters(TaskParams { upid: upid.clone() }))
            .await
            .unwrap();
        srv.task_log(Parameters(TaskLogParams {
            upid: upid.clone(),
            start: Some(0),
            limit: Some(10),
            tail: None,
        }))
        .await
        .unwrap();
        // The `tail` path issues an extra probe request; it must be a GET too.
        srv.task_log(Parameters(TaskLogParams {
            upid,
            start: None,
            limit: None,
            tail: Some(10),
        }))
        .await
        .unwrap();
        srv.gc_status().await.unwrap();
        srv.node_status().await.unwrap();

        let reqs = mock.received_requests().await.unwrap();
        // 10 tool calls, plus the tail probe.
        assert_eq!(reqs.len(), 11);
        for req in &reqs {
            assert_eq!(
                req.method.to_string(),
                "GET",
                "{} used a non-GET method — this server must be read-only",
                req.url
            );
            assert!(
                req.body.is_empty(),
                "{} carried a request body — GETs here must be bodyless",
                req.url
            );
            assert!(req.url.path().starts_with("/api2/json/"));
        }
    }
}
