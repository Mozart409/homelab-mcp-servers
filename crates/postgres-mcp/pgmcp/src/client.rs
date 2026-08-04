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
        let wrapped = wrap_query(sql, clamp_limit(limit, self.max_rows));

        let mut tx = self
            .pool
            .begin()
            .await
            .wrap_err("failed to begin transaction")?;

        // Enforce read-only + a statement timeout for the lifetime of this tx.
        // The statements come from `transaction_preamble` so that the read-only
        // guarantee (hard rule §1) is unit-testable without a live database.
        for stmt in transaction_preamble(self.statement_timeout_ms) {
            // Only fixed text plus a numeric value; no caller input reaches here.
            sqlx::query(AssertSqlSafe(stmt.clone()))
                .execute(&mut *tx)
                .await
                .wrap_err_with(|| format!("failed to configure transaction: {stmt}"))?;
        }

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

// ---- Pure query-construction helpers ----------------------------------------
//
// The safety properties of this crate (read-only, statement-timed, row-capped)
// are decided entirely by the strings built below. Keeping them in small pure
// functions means the guarantees can be asserted in unit tests without a live
// database, instead of relying solely on the opt-in integration suite.

/// Clamp a requested row limit into `[1, max_rows]`.
///
/// `max_rows` itself is floored at 1: a misconfigured `PG_MAX_ROWS` of `0` (or
/// a negative value) would otherwise make `i64::clamp` panic with `min > max`,
/// and these servers must not panic on bad config.
pub(crate) fn clamp_limit(requested: i64, max_rows: i64) -> i64 {
    let cap = max_rows.max(1);
    requested.clamp(1, cap)
}

/// The exact statements [`PgClient::fetch_json`] issues before the caller's SQL.
///
/// `SET TRANSACTION READ ONLY` is the hard read-only guarantee; the statement
/// timeout bounds runaway queries. Both are `SET LOCAL`-scoped to the
/// transaction that wraps the query.
pub(crate) fn transaction_preamble(statement_timeout_ms: u64) -> [String; 2] {
    [
        "SET TRANSACTION READ ONLY".to_string(),
        format!("SET LOCAL statement_timeout = {statement_timeout_ms}"),
    ]
}

/// Wrap a caller's `SELECT` so the row cap applies before aggregation and
/// Postgres renders the whole result set as one JSON text value.
pub(crate) fn wrap_query(sql: &str, limit: i64) -> String {
    format!(
        "SELECT coalesce(jsonb_agg(t), '[]'::jsonb)::text \
         FROM (SELECT * FROM ({sql}) AS _q LIMIT {limit}) AS t"
    )
}

// ---- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`Config`] that never needs a reachable database (the pool is lazy).
    fn config(database_url: &str) -> Config {
        Config {
            database_url: database_url.to_string(),
            bind: "127.0.0.1:0".to_string(),
            allowed_hosts: None,
            max_connections: 2,
            statement_timeout_ms: 5_000,
            max_rows: 1_000,
        }
    }

    // `connect_lazy` registers pool bookkeeping with the runtime, so these need
    // a Tokio context even though no connection is opened.
    #[tokio::test]
    async fn new_rejects_a_malformed_connection_url() {
        // Must surface as an `Err`, not a panic: `run()` reports it and exits.
        // (`PgClient` is not `Debug`, so discard the Ok value before unwrapping.)
        let err = PgClient::new(&config("this is not a url"))
            .map(|_| ())
            .expect_err("a malformed connection string must be rejected");
        assert!(
            format!("{err:#}").contains("connection string"),
            "error should explain what was wrong: {err:#}"
        );

        for bad in [
            "",                                      // empty
            "://",                                   // no scheme, no host
            "postgres://user:pass@host:notaport/db", // non-numeric port
            "postgres://host/db?sslmode=nonsense",   // invalid parameter value
        ] {
            assert!(
                PgClient::new(&config(bad)).is_err(),
                "{bad:?} must be rejected as a connection string"
            );
        }
    }

    #[tokio::test]
    async fn new_accepts_a_valid_url_and_carries_the_limits() {
        // `connect_lazy` means no TCP connection is made here, so an unreachable
        // host is fine — that is what keeps the server startable without the DB.
        let client = PgClient::new(&config("postgres://u:p@127.0.0.1:5432/db"))
            .expect("a well-formed connection string must be accepted");
        assert_eq!(client.max_rows, 1_000);
        assert_eq!(client.statement_timeout_ms, 5_000);
    }

    #[test]
    fn clamp_limit_bounds_requests_to_the_configured_cap() {
        assert_eq!(clamp_limit(10, 1_000), 10);
        assert_eq!(clamp_limit(5_000, 1_000), 1_000, "over-cap must be trimmed");
        assert_eq!(clamp_limit(0, 1_000), 1, "zero must become one row");
        assert_eq!(clamp_limit(-5, 1_000), 1, "negatives must become one row");
        assert_eq!(clamp_limit(i64::MAX, 1_000), 1_000);
        assert_eq!(
            clamp_limit(1_000, 1_000),
            1_000,
            "the cap itself is allowed"
        );
    }

    #[test]
    fn clamp_limit_does_not_panic_on_a_degenerate_cap() {
        // `PG_MAX_ROWS=0` is accepted by config parsing, so the clamp must cope
        // rather than panic on `min > max`.
        assert_eq!(clamp_limit(10, 0), 1);
        assert_eq!(clamp_limit(10, -1), 1);
        assert_eq!(clamp_limit(i64::MIN, i64::MIN), 1);
    }

    #[test]
    fn preamble_enforces_read_only_and_the_statement_timeout() {
        let preamble = transaction_preamble(250);
        assert_eq!(
            preamble,
            [
                "SET TRANSACTION READ ONLY".to_string(),
                "SET LOCAL statement_timeout = 250".to_string(),
            ],
            "hard rule §1: every query must run in a READ ONLY transaction"
        );
    }

    #[test]
    fn preamble_reflects_the_configured_timeout() {
        let [_, timeout] = transaction_preamble(1_234);
        assert_eq!(timeout, "SET LOCAL statement_timeout = 1234");
        let [_, timeout] = transaction_preamble(0);
        assert_eq!(timeout, "SET LOCAL statement_timeout = 0");
    }

    #[test]
    fn wrap_query_applies_the_row_cap_and_json_aggregation() {
        let wrapped = wrap_query("SELECT 1", 25);
        assert!(wrapped.contains("LIMIT 25"), "row cap missing: {wrapped}");
        assert!(
            wrapped.contains("(SELECT 1)"),
            "inner sql missing: {wrapped}"
        );
        assert!(
            wrapped.contains("jsonb_agg"),
            "aggregation missing: {wrapped}"
        );
        assert!(
            wrapped.starts_with("SELECT "),
            "the outer statement must itself be a SELECT: {wrapped}"
        );
    }

    #[test]
    fn wrap_query_never_emits_an_unbounded_limit() {
        // Whatever the caller asks for, the emitted SQL carries the clamped cap.
        for requested in [-1, 0, 1, 10, i64::MAX] {
            let limit = clamp_limit(requested, 50);
            let wrapped = wrap_query("SELECT 1", limit);
            assert!(
                wrapped.contains(&format!("LIMIT {limit}")),
                "expected LIMIT {limit} in {wrapped}"
            );
            assert!((1..=50).contains(&limit), "limit {limit} escaped the cap");
        }
    }
}
