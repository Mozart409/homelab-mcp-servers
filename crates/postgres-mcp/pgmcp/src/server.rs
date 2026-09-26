//! MCP server: exposes a Postgres database as read-only introspection and
//! query tools.

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
use serde::Deserialize;

use crate::client::{PgClient, clamp_limit};

/// MCP server wrapping a [`PgClient`].
#[derive(Clone)]
pub struct PgServer {
    client: PgClient,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl PgServer {
    /// Wrap a configured client as an MCP server.
    #[must_use]
    pub fn new(client: PgClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
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

// ---- Prompt parameter types ---

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SchemaOverviewArgs {
    /// Schema to analyze (default: all non-system schemas).
    #[serde(default)]
    schema: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TableHealthArgs {
    /// Schema-qualified table name, e.g. `public.users` or just `users` (defaults
    /// to `public`).
    table: String,
    /// Schema the table lives in (default: `public`).
    #[serde(default)]
    schema: Option<String>,
}

// ---- Parameter resolution ----------------------------------------------------
//
// Every tool below is a fixed SQL string plus bound parameters, so the only
// per-call logic is how the optional parameters are defaulted and how the row
// limit is resolved. Those are pulled out here as small pure functions.

/// Resolve an optional `schema` argument into a `LIKE` pattern.
///
/// Absent means "every user schema", which `%` expresses — the value is bound
/// as `$1`, never interpolated, so a caller cannot smuggle SQL through it.
fn schema_like_pattern(schema: Option<String>) -> String {
    schema.unwrap_or_else(|| "%".to_string())
}

/// Resolve an optional `schema` argument for exact-match lookups.
///
/// Defaults to `public`, matching Postgres' own default search path.
fn schema_or_public(schema: Option<String>) -> String {
    schema.unwrap_or_else(|| "public".to_string())
}

/// Resolve the effective row limit for a tool call.
///
/// An absent `limit` means "as many as the server allows"; whatever the caller
/// asks for is then clamped into `[1, max_rows]`, so `PG_MAX_ROWS` is a hard
/// cap and not merely a default.
fn effective_limit(requested: Option<i64>, max_rows: i64) -> i64 {
    clamp_limit(requested.unwrap_or(max_rows), max_rows)
}

// ---- Tool SQL ---------------------------------------------------------------
//
// Hoisted out of the tool bodies so every statement the server can issue is in
// one place. They run inside the same READ ONLY transaction as `run_query`.

const SQL_LIST_SCHEMAS: &str = "SELECT schema_name \
     FROM information_schema.schemata \
     WHERE schema_name NOT IN ('pg_catalog', 'information_schema') \
       AND schema_name NOT LIKE 'pg_temp%' \
       AND schema_name NOT LIKE 'pg_toast%' \
     ORDER BY schema_name";

const SQL_LIST_TABLES: &str = "SELECT table_schema, table_name, table_type \
     FROM information_schema.tables \
     WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
       AND table_schema LIKE $1 \
     ORDER BY table_schema, table_name";

const SQL_DESCRIBE_TABLE: &str = "SELECT column_name, ordinal_position, data_type, is_nullable, \
            column_default, character_maximum_length, \
            numeric_precision, numeric_scale \
     FROM information_schema.columns \
     WHERE table_schema = $1 AND table_name = $2 \
     ORDER BY ordinal_position";

const SQL_LIST_INDEXES: &str = "SELECT indexname, indexdef \
     FROM pg_indexes \
     WHERE schemaname = $1 AND tablename = $2 \
     ORDER BY indexname";

const SQL_LIST_FOREIGN_KEYS: &str = "SELECT tc.constraint_name, kcu.column_name, \
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

const SQL_TABLE_STATS: &str = "SELECT schemaname, relname AS table_name, n_live_tup AS estimated_rows, \
            n_dead_tup AS dead_rows, seq_scan, idx_scan, \
            pg_size_pretty(pg_total_relation_size(relid)) AS total_size, \
            last_vacuum, last_autovacuum, last_analyze \
     FROM pg_stat_user_tables \
     WHERE schemaname LIKE $1 \
     ORDER BY pg_total_relation_size(relid) DESC";

const SQL_DATABASE_SIZE: &str = "SELECT current_database() AS database, \
            pg_size_pretty(pg_database_size(current_database())) AS size, \
            pg_database_size(current_database()) AS size_bytes";

// ---- Tools ------------------------------------------------------------------

#[tool_router]
impl PgServer {
    #[tool(description = "List user schemas (excludes system schemas like pg_catalog).")]
    async fn list_schemas(&self) -> Result<String, ErrorData> {
        self.call(SQL_LIST_SCHEMAS, &[], self.client.max_rows).await
    }

    #[tool(
        description = "List tables and views with their schema and type. Optionally restrict to a single schema."
    )]
    async fn list_tables(
        &self,
        Parameters(SchemaFilterParams { schema }): Parameters<SchemaFilterParams>,
    ) -> Result<String, ErrorData> {
        let filter = schema_like_pattern(schema);
        self.call(SQL_LIST_TABLES, &[&filter], self.client.max_rows)
            .await
    }

    #[tool(
        description = "Describe a table's columns: name, position, data type, nullability, default, and length/precision."
    )]
    async fn describe_table(
        &self,
        Parameters(TableParams { schema, table }): Parameters<TableParams>,
    ) -> Result<String, ErrorData> {
        let schema = schema_or_public(schema);
        self.call(SQL_DESCRIBE_TABLE, &[&schema, &table], self.client.max_rows)
            .await
    }

    #[tool(description = "List indexes on a table, with their definitions.")]
    async fn list_indexes(
        &self,
        Parameters(TableParams { schema, table }): Parameters<TableParams>,
    ) -> Result<String, ErrorData> {
        let schema = schema_or_public(schema);
        self.call(SQL_LIST_INDEXES, &[&schema, &table], self.client.max_rows)
            .await
    }

    #[tool(
        description = "List a table's foreign keys: constraint name, local column, and the referenced table/column."
    )]
    async fn list_foreign_keys(
        &self,
        Parameters(TableParams { schema, table }): Parameters<TableParams>,
    ) -> Result<String, ErrorData> {
        let schema = schema_or_public(schema);
        self.call(
            SQL_LIST_FOREIGN_KEYS,
            &[&schema, &table],
            self.client.max_rows,
        )
        .await
    }

    #[tool(
        description = "Per-table statistics: estimated live/dead rows, scan counts, total on-disk size, and last (auto)vacuum/analyze times. Optionally restrict to a single schema."
    )]
    async fn table_stats(
        &self,
        Parameters(SchemaFilterParams { schema }): Parameters<SchemaFilterParams>,
    ) -> Result<String, ErrorData> {
        let filter = schema_like_pattern(schema);
        self.call(SQL_TABLE_STATS, &[&filter], self.client.max_rows)
            .await
    }

    #[tool(description = "Current database name and total on-disk size (pretty and in bytes).")]
    async fn database_size(&self) -> Result<String, ErrorData> {
        self.call(SQL_DATABASE_SIZE, &[], 1).await
    }

    #[tool(
        description = "Run an arbitrary read-only SELECT and return the rows as JSON. The query executes in a READ ONLY transaction with a statement timeout; writes, DDL, and long-running queries are rejected. Results are capped at the configured row limit."
    )]
    async fn run_query(
        &self,
        Parameters(QueryParams { sql, limit }): Parameters<QueryParams>,
    ) -> Result<String, ErrorData> {
        let limit = effective_limit(limit, self.client.max_rows);
        self.call(&sql, &[], limit).await
    }
}

// ---- Prompts ----

/// Common database introspection workflows, encoded as prompts.
///
/// These live in their own inherent `impl` block so `#[prompt_router]` and
/// `#[tool_router]` each own one block outright. Both generate an associated
/// router constructor (`Self::prompt_router()` / `Self::tool_router()`), and
/// keeping them separate avoids asking either macro to walk attributes it does
/// not recognise.
#[prompt_router]
impl PgServer {
    /// Walk through database structure to understand what tables dominate by
    /// size and row count, infer entity relationships from foreign keys, and
    /// flag orphaned or append-only tables.
    #[prompt(
        name = "schema_overview",
        description = "Orient yourself in an unfamiliar database: scale, schemas, table distribution, and core entities."
    )]
    async fn schema_overview(&self, params: Parameters<SchemaOverviewArgs>) -> Vec<PromptMessage> {
        let SchemaOverviewArgs { schema } = params.0;

        let schema_hint = schema.as_ref().map_or_else(
            || "the database".to_string(),
            |s| format!("the `{s}` schema"),
        );

        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Analyze {schema_hint} and help me understand its structure and data distribution.\n\n\
                 Work in this order:\n\
                 1. Start with `database_size` to understand the overall scale.\n\
                 2. Call `list_schemas` to see all available schemas (if not already scoped to one).\n\
                 3. Call `list_tables` to list the tables in the schema{} \n\
                 4. Call `table_stats` to find the largest tables by disk size and by row count. These are rarely the same set — the difference tells a story.\n\
                 5. Call `describe_table` and `list_foreign_keys` on the top 3–4 tables that dominate by size or row count. Use foreign keys to infer real entity relationships rather than guessing from table names.\n\
                 6. Flag any tables that appear orphaned (no incoming or outgoing foreign keys) and any that look like append-only logs (monotonically increasing row counts, no deletes, minimal indexing).\n\n\
                 **Output requirements:**\n\
                 - Name the tables dominating by size and by row count (with row counts and sizes).\n\
                 - Describe the core entity relationships inferred from foreign keys. Do not rely on table names alone.\n\
                 - Explicitly flag tables that are orphaned or appear to be logs.\n\
                 - **If the schema is too large to summarise fully, say so and scope your analysis to the top tables by size instead of guessing.**",
                if schema.is_some() {
                    " you specified"
                } else {
                    ""
                }
            ),
        )]
    }

    /// Identify bloat, redundancy, missing indexes, and data type issues in
    /// a single table to surface problems that operators can fix with schema
    /// changes.
    #[prompt(
        name = "table_health",
        description = "Audit a table's bloat, indexing, referential load, and data type usage."
    )]
    async fn table_health(&self, params: Parameters<TableHealthArgs>) -> Vec<PromptMessage> {
        let TableHealthArgs { table, schema } = params.0;
        let schema = schema.unwrap_or_else(|| "public".to_string());

        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Audit the health of the `{schema}`.`{table}` table. Focus on bloat, indexing, referential load, and data type correctness.\n\n\
                 Work in this order:\n\
                 1. Call `describe_table` to see column names, types, and nullability.\n\
                 2. Call `table_stats` for live/dead tuple counts and on-disk size. **A high dead-tuple ratio (dead_rows / estimated_rows) indicates bloat** — autovacuum settings are the fix, which must be applied by a human. This server cannot run `VACUUM` or `REINDEX`.\n\
                 3. Call `list_indexes` to see what indexes exist.\n\
                 4. Call `list_foreign_keys` to see the referential load: both foreign keys referencing this table and foreign keys pointing outward.\n\
                 5. Use `run_query` for targeted read-only counts if the catalog tools do not show what you need (e.g. NULL distributions, cardinality, duplicate checks). Stay within the row cap and statement timeout.\n\n\
                 **Identify and report:**\n\
                 - **Bloat:** Dead-tuple ratio and what fix to apply (autovacuum tuning, manual `VACUUM FULL`, `REINDEX` — humans must run these out-of-band).\n\
                 - **Index health:** Redundant indexes (one whose leading columns are a prefix of another's), missing indexes on foreign key columns (which slows referential checks and cascading deletes), and unused indexes.\n\
                 - **Referential integrity:** Foreign key columns lacking a supporting index.\n\
                 - **Data type issues:** Columns declared as TEXT or VARCHAR(large) when SMALLINT or UUID would fit; NUMERIC(huge_precision) for what could be DECIMAL(10,2); TIMESTAMP without timezone handling; large arrays when they could be a separate table.\n\n\
                 **Important:** Autovacuum settings, `VACUUM`, `REINDEX`, and schema changes are not available through this server. Flag what humans must fix; do not suggest this server run those commands."
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
impl ServerHandler for PgServer {
    fn get_info(&self) -> ServerConfig {
        // `ServerConfig` is `#[non_exhaustive]`, so build from default and assign.
        let mut info = ServerConfig::default();
        info.instructions = Some(
            "Read-only access to a Postgres database. Use these tools to inspect \
             schemas, tables, columns, indexes, and statistics, or to run ad-hoc \
             read-only SELECT queries via `run_query`."
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
    /// The README documents the read-only transaction guarantee, statement
    /// timeout, and row cap — information no tool return value carries.
    /// Exposing it as a resource lets clients read the reasoning without
    /// burning a tool call on it.
    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let uri = mcp_common::doc_resource_uri(env!("CARGO_PKG_NAME"));
        Ok(ListResourcesResult::with_all_items(vec![
            mcp_common::doc_resource(
                &uri,
                "PostgreSQL MCP operator guide",
                "README for pgmcp: read-only guarantees, transaction limits, and schema introspection.",
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
