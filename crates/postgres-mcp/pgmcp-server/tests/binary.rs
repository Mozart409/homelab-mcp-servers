//! The real `pgmcp-server` executable: env → config → router → socket, against
//! the throwaway cluster `just test` provides.
//!
//! The in-process suite in `pgmcp/tests/e2e.rs` builds `Config` by hand, so
//! this is the only place the `PG_*` variables (including the `DATABASE_URL`
//! fallback and the numeric limits), `main`, and `--healthcheck` run as an
//! operator's container runs them.

use mcp_common::e2e::{
    BinarySpec, McpClient, ServerProcess, check_binary_contract, free_loopback_addr, run_to_exit,
};
use serde_json::json;

const BIN: &str = env!("CARGO_BIN_EXE_pgmcp-server");

const NO_CLUSTER: &str =
    "PGMCP_TEST_DATABASE_URL is not set: run with `just test`, which starts a throwaway cluster";

fn cluster_url() -> Result<String, std::env::VarError> {
    std::env::var("PGMCP_TEST_DATABASE_URL")
}

#[tokio::test]
async fn startup_contract() {
    let url = cluster_url().expect(NO_CLUSTER);
    check_binary_contract(&BinarySpec {
        bin: BIN,
        bind_var: "PG_BIND",
        required: &[("PG_DATABASE_URL", &url)],
        extra: &[],
    })
    .await
    .unwrap();
}

/// `DATABASE_URL` is the documented fallback, and the numeric limits from the
/// environment are the ones enforced: `PG_MAX_ROWS=3` caps a 10-row query.
#[tokio::test]
async fn database_url_fallback_and_env_limits_are_enforced() {
    let url = cluster_url().expect(NO_CLUSTER);
    let bind = free_loopback_addr().unwrap();
    let _server = ServerProcess::spawn(
        BIN,
        &bind,
        &[
            ("DATABASE_URL", &url),
            ("PG_MAX_ROWS", "3"),
            ("PG_STATEMENT_TIMEOUT_MS", "500"),
            ("PG_BIND", &bind),
        ],
    )
    .await
    .unwrap();
    let client = McpClient::connect(&format!("http://{bind}/mcp"))
        .await
        .unwrap();

    let rows = client
        .call_tool(
            "run_query",
            json!({ "sql": "SELECT g FROM generate_series(1, 10) AS g" }),
        )
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(rows, json!([{ "g": 1 }, { "g": 2 }, { "g": 3 }]));

    let slow = client
        .call_tool("run_query", json!({ "sql": "SELECT pg_sleep(5)" }))
        .await
        .unwrap();
    assert!(slow.error_message().unwrap().contains("statement timeout"));

    let written = client
        .call_tool("run_query", json!({ "sql": "SELECT lo_create(0)" }))
        .await
        .unwrap();
    assert!(
        written
            .error_message()
            .unwrap()
            .contains("read-only transaction")
    );
}

/// A non-numeric limit is a startup error that names the variable, not a
/// silent fallback to the default.
#[tokio::test]
async fn unparseable_limits_refuse_to_start() {
    let url = cluster_url().expect(NO_CLUSTER);
    let bind = free_loopback_addr().unwrap();
    let out = run_to_exit(
        BIN,
        &[
            ("PG_DATABASE_URL", &url),
            ("PG_MAX_ROWS", "lots"),
            ("PG_BIND", &bind),
        ],
        &[],
    )
    .await
    .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("PG_MAX_ROWS"));
}
