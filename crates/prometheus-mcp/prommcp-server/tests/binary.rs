//! The real `prommcp-server` executable: env → config → router → socket.

use mcp_common::e2e::{
    BinarySpec, McpClient, ServerProcess, check_binary_contract, free_loopback_addr,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = env!("CARGO_BIN_EXE_prommcp-server");

#[tokio::test]
async fn startup_contract() {
    let mock = MockServer::start().await;
    check_binary_contract(&BinarySpec {
        bin: BIN,
        bind_var: "PROM_BIND",
        required: &[("PROM_HOST", &mock.uri())],
        extra: &[],
    })
    .await
    .unwrap();
}

/// `PROM_TOKEN` becomes a bearer header; an empty `PROM_TOKEN` means none.
#[tokio::test]
async fn token_from_env_reaches_prometheus_and_empty_means_unset() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/status/buildinfo"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "status": "success", "data": { "version": "3.1.0" } })),
        )
        .mount(&mock)
        .await;
    let uri = mock.uri();

    for token in ["prom-reader-7f3a9c", ""] {
        let bind = free_loopback_addr().unwrap();
        let _server = ServerProcess::spawn(
            BIN,
            &bind,
            &[
                ("PROM_HOST", &uri),
                ("PROM_TOKEN", token),
                ("PROM_BIND", &bind),
            ],
        )
        .await
        .unwrap();
        let client = McpClient::connect(&format!("http://{bind}/mcp"))
            .await
            .unwrap();
        client
            .call_tool("build_info", json!({}))
            .await
            .unwrap()
            .text()
            .unwrap();
    }
    // First request carries the bearer token, second carries none.
    let seen = mcp_common::e2e::upstream_requests(&mock).await.unwrap();
    insta::assert_json_snapshot!("binary_token_handling", seen);
}
