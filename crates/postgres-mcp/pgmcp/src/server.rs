//! MCP server: exposes a Postgres database as read-only introspection and
//! query tools.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::client::{PgClient, clamp_limit};

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

// ---- Parameter resolution ----------------------------------------------------
//
// Every tool below is a fixed SQL string plus bound parameters, so the only
// per-call logic is how the optional parameters are defaulted and how the row
// limit is resolved. Those are pulled out here as pure functions so they are
// testable without a live database.

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
// Hoisted out of the tool bodies so the read-only property of every statement
// can be asserted in unit tests (see `all_tool_sql_is_read_only`).

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

/// Every fixed statement the tools issue, for the read-only assertion tests.
#[cfg(test)]
const ALL_TOOL_SQL: &[&str] = &[
    SQL_LIST_SCHEMAS,
    SQL_LIST_TABLES,
    SQL_DESCRIBE_TABLE,
    SQL_LIST_INDEXES,
    SQL_LIST_FOREIGN_KEYS,
    SQL_TABLE_STATS,
    SQL_DATABASE_SIZE,
];

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

// ---- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use crate::client::{transaction_preamble, wrap_query};

    #[test]
    fn schema_filter_defaults_to_every_user_schema() {
        assert_eq!(schema_like_pattern(None), "%");
        assert_eq!(schema_like_pattern(Some("public".to_string())), "public");
        // A caller-supplied pattern is passed through verbatim; it is bound as
        // `$1`, so LIKE metacharacters affect matching only, never parsing.
        assert_eq!(schema_like_pattern(Some("app_%".to_string())), "app_%");
        assert_eq!(
            schema_like_pattern(Some("'; DROP SCHEMA x; --".to_string())),
            "'; DROP SCHEMA x; --",
            "hostile input must survive verbatim as a bound value"
        );
        // An explicit empty string is honoured (matches nothing), not silently
        // widened back to `%`.
        assert_eq!(schema_like_pattern(Some(String::new())), "");
    }

    #[test]
    fn table_lookups_default_to_the_public_schema() {
        assert_eq!(schema_or_public(None), "public");
        assert_eq!(schema_or_public(Some("app".to_string())), "app");
        assert_eq!(schema_or_public(Some(String::new())), "");
    }

    #[test]
    fn effective_limit_defaults_to_the_configured_cap() {
        assert_eq!(effective_limit(None, 1_000), 1_000);
        assert_eq!(effective_limit(None, 7), 7);
    }

    #[test]
    fn effective_limit_treats_max_rows_as_a_hard_cap() {
        assert_eq!(effective_limit(Some(10), 1_000), 10);
        assert_eq!(
            effective_limit(Some(5_000), 1_000),
            1_000,
            "a caller must not be able to exceed PG_MAX_ROWS"
        );
        assert_eq!(effective_limit(Some(i64::MAX), 1_000), 1_000);
    }

    #[test]
    fn effective_limit_floors_degenerate_requests_without_panicking() {
        assert_eq!(effective_limit(Some(0), 1_000), 1);
        assert_eq!(effective_limit(Some(-1), 1_000), 1);
        assert_eq!(effective_limit(Some(i64::MIN), 1_000), 1);
        // A misconfigured cap must not panic the daemon.
        assert_eq!(effective_limit(None, 0), 1);
        assert_eq!(effective_limit(Some(10), -5), 1);
    }

    /// Hard rule §1: none of the fixed tool statements may mutate the target.
    #[test]
    fn all_tool_sql_is_read_only() {
        const FORBIDDEN: &[&str] = &[
            "INSERT", "UPDATE", "DELETE", "DROP", "CREATE", "ALTER", "TRUNCATE", "GRANT", "REVOKE",
            "COPY", "MERGE", "CALL", "VACUUM", "ANALYZE", "REINDEX", "REFRESH", "LOCK", "SET",
        ];

        for sql in ALL_TOOL_SQL {
            assert!(
                sql.starts_with("SELECT "),
                "every tool statement must be a SELECT: {sql}"
            );
            // Compare whole identifiers, not substrings: column names such as
            // `last_vacuum` and `last_analyze` are legitimate reads.
            for token in sql.split(|c: char| !c.is_alphanumeric() && c != '_') {
                let token = token.to_uppercase();
                assert!(
                    !FORBIDDEN.contains(&token.as_str()),
                    "tool SQL contains the mutating keyword {token:?}: {sql}"
                );
            }
        }
    }

    /// The `run_query` path: caller SQL is wrapped, capped, and executed behind
    /// the read-only preamble. Proven here without a database by exercising the
    /// exact helpers `PgClient::fetch_json` uses.
    #[test]
    fn run_query_path_is_read_only_capped_and_time_limited() {
        let max_rows = 100;
        let statement_timeout_ms = 5_000;
        let caller_sql = "SELECT * FROM users";

        let limit = effective_limit(Some(10_000), max_rows);
        assert_eq!(limit, max_rows, "the caller's limit must be capped");

        let [read_only, timeout] = transaction_preamble(statement_timeout_ms);
        assert_eq!(read_only, "SET TRANSACTION READ ONLY");
        assert_eq!(timeout, "SET LOCAL statement_timeout = 5000");

        let wrapped = wrap_query(caller_sql, limit);
        assert!(wrapped.contains(caller_sql), "caller sql lost: {wrapped}");
        assert!(wrapped.contains("LIMIT 100"), "cap not applied: {wrapped}");
    }
}
