//! Thin async wrapper around a Postgres connection pool.
//!
//! Every query the MCP tools run goes through [`PgClient::fetch_json`], which
//! executes inside a `READ ONLY` transaction with a per-statement timeout and a
//! row cap, and returns the result already serialized to a JSON array (rendered
//! by Postgres via `jsonb_agg`). Running queries read-only is the core safety
//! property: writes and DDL fail at execution time rather than mutating the DB.

use color_eyre::eyre::{Result, WrapErr};
use sqlx::AssertSqlSafe;
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::Config;

/// Connection handle for a single Postgres instance.
///
/// Cheap to clone ([`PgPool`] is reference-counted internally).
#[derive(Clone)]
pub struct PgClient {
    pool: PgPool,
    /// Per-statement timeout in milliseconds.
    statement_timeout_ms: u64,
    /// Hard cap on rows returned by any single call.
    pub max_rows: i64,
}

impl PgClient {
    /// Build a lazily-connected pool from configuration. The first query
    /// establishes the actual TCP connection, so an unreachable database does
    /// not prevent the MCP server from starting.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection string is malformed.
    pub fn new(config: &Config) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .connect_lazy(&config.database_url)
            .wrap_err("invalid PG_DATABASE_URL / DATABASE_URL connection string")?;

        Ok(Self {
            pool,
            statement_timeout_ms: config.statement_timeout_ms,
            max_rows: config.max_rows,
        })
    }

    /// Run `sql` (a single `SELECT`-like statement) inside a read-only,
    /// time-limited transaction and return up to `limit` rows as a JSON array
    /// string.
    ///
    /// `params` are bound positionally as text (`$1`, `$2`, …) — use them for
    /// any caller-supplied values so they are never interpolated into SQL.
    /// `limit` is clamped to `[1, max_rows]`.
    ///
    /// # Errors
    ///
    /// Returns an error if the statement is not read-only, exceeds the
    /// statement timeout, or otherwise fails to execute.
    pub async fn fetch_json(&self, sql: &str, params: &[&str], limit: i64) -> Result<String> {
        let limit = limit.clamp(1, self.max_rows);

        // Wrap the inner query so the row cap is applied before aggregation, then
        // let Postgres serialize the whole result set to a single JSON text value.
        let wrapped = format!(
            "SELECT coalesce(jsonb_agg(t), '[]'::jsonb)::text \
             FROM (SELECT * FROM ({sql}) AS _q LIMIT {limit}) AS t"
        );

        let mut tx = self
            .pool
            .begin()
            .await
            .wrap_err("failed to begin transaction")?;

        // Enforce read-only + a statement timeout for the lifetime of this tx.
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *tx)
            .await
            .wrap_err("failed to set transaction read-only")?;
        // Only a numeric value is interpolated here.
        sqlx::query(AssertSqlSafe(format!(
            "SET LOCAL statement_timeout = {}",
            self.statement_timeout_ms
        )))
        .execute(&mut *tx)
        .await
        .wrap_err("failed to set statement timeout")?;

        // `sql` is the deliberate ad-hoc query surface; it is sandboxed by the
        // READ ONLY transaction and statement timeout above, not by escaping.
        // Caller-supplied *values* are passed via `params` bind parameters.
        let mut query = sqlx::query(AssertSqlSafe(wrapped));
        for p in params {
            query = query.bind(*p);
        }
        let row = query
            .fetch_one(&mut *tx)
            .await
            .wrap_err("query execution failed")?;
        let json: String = row.try_get(0).wrap_err("failed to read query result")?;

        // Read-only transaction: nothing to commit, just release the connection.
        let _ = tx.rollback().await;

        Ok(json)
    }
}
