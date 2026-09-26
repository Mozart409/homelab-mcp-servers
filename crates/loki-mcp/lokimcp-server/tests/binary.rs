//! The real `lokimcp-server` executable: env → config → router → socket.

use mcp_common::e2e::{
    BinarySpec, McpClient, ServerProcess, check_binary_contract, free_loopback_addr,
    upstream_requests,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = env!("CARGO_BIN_EXE_lokimcp-server");

#[tokio::test]
async fn startup_contract() {
    let mock = MockServer::start().await;
    check_binary_contract(&BinarySpec {
        bin: BIN,
        bind_var: "LOKI_BIND",
        required: &[("LOKI_HOST", &mock.uri())],
        extra: &[],
    })
    .await
    .unwrap();
}

/// `LOKI_ORG_ID` and `LOKI_TOKEN` reach Loki as headers; set to the empty
/// string they are treated as unset, not sent as empty headers.
#[tokio::test]
async fn tenant_and_token_from_env_and_empty_means_unset() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/labels"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "status": "success", "data": [] })),
        )
        .mount(&mock)
        .await;
    let uri = mock.uri();

    for (org, token) in [("homelab", "loki-reader-51c0"), ("", "")] {
        let bind = free_loopback_addr().unwrap();
        let _server = ServerProcess::spawn(
            BIN,
            &bind,
            &[
                ("LOKI_HOST", &uri),
                ("LOKI_ORG_ID", org),
                ("LOKI_TOKEN", token),
                ("LOKI_BIND", &bind),
            ],
        )
        .await
        .unwrap();
        let client = McpClient::connect(&format!("http://{bind}/mcp"))
            .await
            .unwrap();
        client
            .call_tool("labels", json!({}))
            .await
            .unwrap()
            .text()
            .unwrap();
    }
    insta::assert_json_snapshot!(
        "binary_tenant_and_token",
        upstream_requests(&mock).await.unwrap()
    );
}
