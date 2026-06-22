//! MCP server: exposes PBS backup-status data as read-only tools.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::client::PbsClient;

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
    /// Maximum number of log lines to return (default: 100).
    #[serde(default)]
    limit: Option<u64>,
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
        self.call(&format!("/admin/datastore/{store}/status"), &[])
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
        self.call(&format!("/admin/datastore/{store}/groups"), &q)
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
        self.call(&format!("/admin/datastore/{store}/snapshots"), &q)
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
        self.call(&format!("/nodes/{node}/tasks"), &q).await
    }

    #[tool(
        description = "Get the status of a single task by its UPID (running/stopped, exit status)."
    )]
    async fn task_status(
        &self,
        Parameters(TaskParams { upid }): Parameters<TaskParams>,
    ) -> Result<String, ErrorData> {
        let node = self.client.node.clone();
        self.call(&format!("/nodes/{node}/tasks/{upid}/status"), &[])
            .await
    }

    #[tool(description = "Read the log output of a task by its UPID.")]
    async fn task_log(
        &self,
        Parameters(TaskLogParams { upid, limit }): Parameters<TaskLogParams>,
    ) -> Result<String, ErrorData> {
        let node = self.client.node.clone();
        let q = vec![("limit", limit.unwrap_or(100).to_string())];
        self.call(&format!("/nodes/{node}/tasks/{upid}/log"), &q)
            .await
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
        self.call(&format!("/nodes/{node}/status"), &[]).await
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
