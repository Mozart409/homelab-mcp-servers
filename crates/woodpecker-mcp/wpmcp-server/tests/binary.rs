//! The real `wpmcp-server` executable: env → config → router → socket.

use mcp_common::e2e::{
    BinarySpec, McpClient, ServerProcess, check_binary_contract, free_loopback_addr, run_to_exit,
    upstream_requests,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = env!("CARGO_BIN_EXE_wpmcp-server");
const TOKEN: &str = "wp-pat-4c1d8e2f";

#[tokio::test]
async fn startup_contract() {
    let mock = MockServer::start().await;
    check_binary_contract(&BinarySpec {
        bin: BIN,
        bind_var: "WP_BIND",
        required: &[("WP_HOST", &mock.uri()), ("WP_TOKEN", TOKEN)],
        extra: &[],
    })
    .await
    .unwrap();
}

/// A present-but-empty `WP_TOKEN` is a startup error: it would otherwise send
/// `Authorization: Bearer ` and fail every call with a confusing 401.
#[tokio::test]
async fn empty_token_refuses_to_start() {
    let mock = MockServer::start().await;
    let bind = free_loopback_addr().unwrap();
    let uri = mock.uri();
    let out = run_to_exit(
        BIN,
        &[("WP_HOST", &uri), ("WP_TOKEN", ""), ("WP_BIND", &bind)],
        &[],
    )
    .await
    .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("WP_TOKEN"));
}

/// `WP_MAX_LOG_LINES` from the environment is the cap `step_logs` applies,
/// and the token reaches Woodpecker as a bearer header.
#[tokio::test]
async fn env_log_cap_and_token_are_applied() {
    let mock = MockServer::start().await;
    let entries: Vec<_> = (1..=4)
        .map(|n| json!({ "line": n, "data": "bGluZQ==" }))
        .collect();
    Mock::given(method("GET"))
        .and(path("/api/repos/7/logs/1/2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(entries))
        .mount(&mock)
        .await;
    let uri = mock.uri();
    let bind = free_loopback_addr().unwrap();
    let _server = ServerProcess::spawn(
        BIN,
        &bind,
        &[
            ("WP_HOST", &uri),
            ("WP_TOKEN", TOKEN),
            ("WP_MAX_LOG_LINES", "3"),
            ("WP_BIND", &bind),
        ],
    )
    .await
    .unwrap();
    let client = McpClient::connect(&format!("http://{bind}/mcp"))
        .await
        .unwrap();

    let logs = client
        .call_tool(
            "step_logs",
            json!({ "repo_id": 7, "number": 1, "step_id": 2 }),
        )
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(logs.get("returned_entries"), Some(&json!(3)));
    insta::assert_json_snapshot!(
        "binary_token_and_cap",
        upstream_requests(&mock).await.unwrap()
    );
}
