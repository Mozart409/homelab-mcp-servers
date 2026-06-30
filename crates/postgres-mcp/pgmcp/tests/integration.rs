//! Integration tests for [`PgClient`] against a live Postgres.
//!
//! These run only when `PGMCP_TEST_DATABASE_URL` is set, for example using the
//! `postgres` service from `compose.yaml`:
//!
//! ```sh
//! PGMCP_TEST_DATABASE_URL=postgres://pgmcp:change-me@127.0.0.1:5432/pgmcp \
//!   cargo test -p pgmcp --test integration
//! ```
//!
//! When the variable is unset each test logs a skip notice and passes, so the
//! suite stays green in environments without a database.

use pgmcp::{Config, PgClient};

/// Connection string for the test database, or `None` to skip.
fn test_url() -> Option<String> {
    std::env::var("PGMCP_TEST_DATABASE_URL").ok()
}

/// A [`Config`] pointing at `url` with small, test-friendly limits.
fn test_config(url: String, statement_timeout_ms: u64) -> Config {
    Config {
        database_url: url,
        bind: "127.0.0.1:0".to_string(),
        allowed_hosts: None,
        max_connections: 2,
        statement_timeout_ms,
        max_rows: 1_000,
    }
}

/// Parse a [`PgClient::fetch_json`] result into a JSON value.
fn parse(json: &str) -> serde_json::Value {
    serde_json::from_str(json).expect("fetch_json must return valid JSON")
}

#[tokio::test]
async fn basic_select_returns_json_rows() {
    let Some(url) = test_url() else {
        eprintln!("skip: PGMCP_TEST_DATABASE_URL unset");
        return;
    };
    let client = PgClient::new(&test_config(url, 5_000)).unwrap();

    let json = client
        .fetch_json("SELECT 1 AS one, 'hi' AS greeting", &[], 10)
        .await
        .unwrap();

    assert_eq!(
        parse(&json),
        serde_json::json!([{ "one": 1, "greeting": "hi" }])
    );
}

#[tokio::test]
async fn row_cap_limits_results() {
    let Some(url) = test_url() else {
        eprintln!("skip: PGMCP_TEST_DATABASE_URL unset");
        return;
    };
    let client = PgClient::new(&test_config(url, 5_000)).unwrap();

    // generate_series yields 100 rows; the limit must trim it to 5.
    let json = client
        .fetch_json("SELECT g FROM generate_series(1, 100) AS g", &[], 5)
        .await
        .unwrap();

    let rows = parse(&json);
    assert_eq!(rows.as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn empty_result_is_an_empty_array() {
    let Some(url) = test_url() else {
        eprintln!("skip: PGMCP_TEST_DATABASE_URL unset");
        return;
    };
    let client = PgClient::new(&test_config(url, 5_000)).unwrap();

    let json = client
        .fetch_json("SELECT 1 WHERE false", &[], 10)
        .await
        .unwrap();

    assert_eq!(parse(&json), serde_json::json!([]));
}

#[tokio::test]
async fn bind_params_are_passed_positionally() {
    let Some(url) = test_url() else {
        eprintln!("skip: PGMCP_TEST_DATABASE_URL unset");
        return;
    };
    let client = PgClient::new(&test_config(url, 5_000)).unwrap();

    // The value arrives as text and is cast in SQL — proving it is bound, not
    // interpolated. A value containing SQL syntax must round-trip verbatim.
    let json = client
        .fetch_json(
            "SELECT $1::int AS v, $2 AS raw",
            &["42", "1; DROP TABLE x"],
            10,
        )
        .await
        .unwrap();

    assert_eq!(
        parse(&json),
        serde_json::json!([{ "v": 42, "raw": "1; DROP TABLE x" }])
    );
}

#[tokio::test]
async fn statement_timeout_aborts_slow_queries() {
    let Some(url) = test_url() else {
        eprintln!("skip: PGMCP_TEST_DATABASE_URL unset");
        return;
    };
    // 100ms budget vs a 2s sleep: must error rather than hang.
    let client = PgClient::new(&test_config(url, 100)).unwrap();

    let res = client.fetch_json("SELECT pg_sleep(2)", &[], 1).await;
    assert!(res.is_err(), "slow query should hit the statement timeout");
}

#[tokio::test]
async fn writes_are_rejected_in_read_only_transaction() {
    let Some(url) = test_url() else {
        eprintln!("skip: PGMCP_TEST_DATABASE_URL unset");
        return;
    };

    // Set up a real table via a separate pool (the client is read-only and
    // cannot create one).
    let pool = sqlx::postgres::PgPool::connect(&url).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS pgmcp_test_ro")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE pgmcp_test_ro (x int)")
        .execute(&pool)
        .await
        .unwrap();

    let client = PgClient::new(&test_config(url, 5_000)).unwrap();

    // A data-modifying CTE is valid in a subquery position, so it reaches
    // execution and is blocked by the READ ONLY transaction.
    let res = client
        .fetch_json(
            "WITH ins AS (INSERT INTO pgmcp_test_ro VALUES (1) RETURNING x) SELECT * FROM ins",
            &[],
            10,
        )
        .await;
    assert!(res.is_err(), "INSERT must be rejected as read-only");

    // And nothing was actually written.
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pgmcp_test_ro")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "read-only transaction must not have inserted");

    sqlx::query("DROP TABLE pgmcp_test_ro")
        .execute(&pool)
        .await
        .unwrap();
}
