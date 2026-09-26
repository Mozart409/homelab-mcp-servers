//! The real `tempomcp-server` executable: env → config → router → socket.

use mcp_common::e2e::{
    BinarySpec, McpClient, ServerProcess, check_binary_contract, free_loopback_addr,
    upstream_requests,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = env!("CARGO_BIN_EXE_tempomcp-server");

#[tokio::test]
async fn startup_contract() {
    let mock = MockServer::start().await;
    check_binary_contract(&BinarySpec {
        bin: BIN,
        bind_var: "TEMPO_BIND",
        required: &[("TEMPO_HOST", &mock.uri())],
        extra: &[],
    })
    .await
    .unwrap();
}

/// `TEMPO_ORG_ID` and `TEMPO_TOKEN` reach Tempo as headers; set to the empty
/// string they are treated as unset, not sent as empty headers. A host given
/// without a scheme gets `http://` (the mock's `host:port` form).
#[tokio::test]
async fn tenant_and_token_from_env_and_empty_means_unset() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "scopes": [] })))
        .mount(&mock)
        .await;
    let host = mock.address().to_string();

    for (org, token) in [("homelab", "otel-query-7f3a"), ("", "")] {
        let bind = free_loopback_addr().unwrap();
        let _server = ServerProcess::spawn(
            BIN,
            &bind,
            &[
                ("TEMPO_HOST", &host),
                ("TEMPO_ORG_ID", org),
                ("TEMPO_TOKEN", token),
                ("TEMPO_BIND", &bind),
            ],
        )
        .await
        .unwrap();
        let client = McpClient::connect(&format!("http://{bind}/mcp"))
            .await
            .unwrap();
        client
            .call_tool("search_tags", json!({}))
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
