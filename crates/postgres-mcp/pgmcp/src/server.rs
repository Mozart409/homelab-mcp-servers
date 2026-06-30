//! MCP server: exposes a Postgres database as read-only introspection and
//! query tools.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::client::PgClient;

/// MCP server wrapping a [`PgClient`].
#[derive(Clone)]
pub struct PgServer {
    client: PgClient,
    tool_router: ToolRouter<Self>,
}

impl PgServer {
    /// Wrap a configured client as an MCP server.
    #[must_use]
    pub fn new(client: PgClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
        }
    }

    /// Run a read-only query through the client, mapping any error into an MCP
    /// error. The returned string is already a JSON array.
    async fn call(&self, sql: &str, params: &[&str], limit: i64) -> Result<String, ErrorData> {
        self.client
            .fetch_json(sql, params, limit)
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))
    }
}

// ---- Tool parameter types ---------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SchemaFilterParams {
    /// Restrict to a single schema (default: all user schemas).
    #[serde(default)]
    schema: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TableParams {
    /// Schema the table lives in (default: `public`).
    #[serde(default)]
    schema: Option<String>,
    /// Table name.
    table: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct QueryParams {
    /// A single read-only `SELECT` statement. Writes and DDL are rejected at
    /// execution time (the query runs in a `READ ONLY` transaction).
    sql: String,
    /// Maximum number of rows to return (default and hard cap: server's
    /// `PG_MAX_ROWS`).
    #[serde(default)]
    limit: Option<i64>,
}

// ---- Tools ------------------------------------------------------------------

#[tool_router]
impl PgServer {
    #[tool(description = "List user schemas (excludes system schemas like pg_catalog).")]
    async fn list_schemas(&self) -> Result<String, ErrorData> {
        let sql = "SELECT schema_name \
                   FROM information_schema.schemata \
                   WHERE schema_name NOT IN ('pg_catalog', 'information_schema') \
                     AND schema_name NOT LIKE 'pg_temp%' \
                     AND schema_name NOT LIKE 'pg_toast%' \
                   ORDER BY schema_name";
        self.call(sql, &[], self.client.max_rows).await
    }

    #[tool(
        description = "List tables and views with their schema and type. Optionally restrict to a single schema."
    )]
    async fn list_tables(
        &self,
        Parameters(SchemaFilterParams { schema }): Parameters<SchemaFilterParams>,
    ) -> Result<String, ErrorData> {
        let filter = schema.unwrap_or_else(|| "%".to_string());
        let sql = "SELECT table_schema, table_name, table_type \
                   FROM information_schema.tables \
                   WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
                     AND table_schema LIKE $1 \
                   ORDER BY table_schema, table_name";
        self.call(sql, &[&filter], self.client.max_rows).await
    }

    #[tool(
        description = "Describe a table's columns: name, position, data type, nullability, default, and length/precision."
    )]
    async fn describe_table(
        &self,
        Parameters(TableParams { schema, table }): Parameters<TableParams>,
    ) -> Result<String, ErrorData> {
        let schema = schema.unwrap_or_else(|| "public".to_string());
        let sql = "SELECT column_name, ordinal_position, data_type, is_nullable, \
                          column_default, character_maximum_length, \
                          numeric_precision, numeric_scale \
                   FROM information_schema.columns \
                   WHERE table_schema = $1 AND table_name = $2 \
                   ORDER BY ordinal_position";
        self.call(sql, &[&schema, &table], self.client.max_rows)
            .await
    }

    #[tool(description = "List indexes on a table, with their definitions.")]
    async fn list_indexes(
        &self,
        Parameters(TableParams { schema, table }): Parameters<TableParams>,
    ) -> Result<String, ErrorData> {
        let schema = schema.unwrap_or_else(|| "public".to_string());
        let sql = "SELECT indexname, indexdef \
                   FROM pg_indexes \
                   WHERE schemaname = $1 AND tablename = $2 \
                   ORDER BY indexname";
        self.call(sql, &[&schema, &table], self.client.max_rows)
            .await
    }

    #[tool(
        description = "List a table's foreign keys: constraint name, local column, and the referenced table/column."
    )]
    async fn list_foreign_keys(
        &self,
        Parameters(TableParams { schema, table }): Parameters<TableParams>,
    ) -> Result<String, ErrorData> {
        let schema = schema.unwrap_or_else(|| "public".to_string());
        let sql = "SELECT tc.constraint_name, kcu.column_name, \
                          ccu.table_schema AS foreign_table_schema, \
                          ccu.table_name   AS foreign_table_name, \
                          ccu.column_name  AS foreign_column_name \
                   FROM information_schema.table_constraints tc \
                   JOIN information_schema.key_column_usage kcu \
                     ON tc.constraint_name = kcu.constraint_name \
                    AND tc.table_schema = kcu.table_schema \
                   JOIN information_schema.constraint_column_usage ccu \
                     ON ccu.constraint_name = tc.constraint_name \
                    AND ccu.table_schema = tc.table_schema \
                   WHERE tc.constraint_type = 'FOREIGN KEY' \
                     AND tc.table_schema = $1 AND tc.table_name = $2 \
                   ORDER BY tc.constraint_name, kcu.column_name";
        self.call(sql, &[&schema, &table], self.client.max_rows)
            .await
    }

    #[tool(
        description = "Per-table statistics: estimated live/dead rows, scan counts, total on-disk size, and last (auto)vacuum/analyze times. Optionally restrict to a single schema."
    )]
    async fn table_stats(
        &self,
        Parameters(SchemaFilterParams { schema }): Parameters<SchemaFilterParams>,
    ) -> Result<String, ErrorData> {
        let filter = schema.unwrap_or_else(|| "%".to_string());
        let sql = "SELECT schemaname, relname AS table_name, n_live_tup AS estimated_rows, \
                          n_dead_tup AS dead_rows, seq_scan, idx_scan, \
                          pg_size_pretty(pg_total_relation_size(relid)) AS total_size, \
                          last_vacuum, last_autovacuum, last_analyze \
                   FROM pg_stat_user_tables \
                   WHERE schemaname LIKE $1 \
                   ORDER BY pg_total_relation_size(relid) DESC";
        self.call(sql, &[&filter], self.client.max_rows).await
    }

    #[tool(description = "Current database name and total on-disk size (pretty and in bytes).")]
    async fn database_size(&self) -> Result<String, ErrorData> {
        let sql = "SELECT current_database() AS database, \
                          pg_size_pretty(pg_database_size(current_database())) AS size, \
                          pg_database_size(current_database()) AS size_bytes";
        self.call(sql, &[], 1).await
    }

    #[tool(
        description = "Run an arbitrary read-only SELECT and return the rows as JSON. The query executes in a READ ONLY transaction with a statement timeout; writes, DDL, and long-running queries are rejected. Results are capped at the configured row limit."
    )]
    async fn run_query(
        &self,
        Parameters(QueryParams { sql, limit }): Parameters<QueryParams>,
    ) -> Result<String, ErrorData> {
        let limit = limit.unwrap_or(self.client.max_rows);
        self.call(&sql, &[], limit).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PgServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`, so build from default and assign.
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Read-only access to a Postgres database. Use these tools to inspect \
             schemas, tables, columns, indexes, and statistics, or to run ad-hoc \
             read-only SELECT queries via `run_query`."
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
