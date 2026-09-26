//! The real `hamcp-server` executable: env → config → router → socket.

use mcp_common::e2e::{
    BinarySpec, McpClient, ServerProcess, TlsProxy, check_binary_contract, free_loopback_addr,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = env!("CARGO_BIN_EXE_hamcp-server");
const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.hamcp-e2e.c2lnbmF0dXJl";

#[tokio::test]
async fn startup_contract() {
    let mock = MockServer::start().await;
    check_binary_contract(&BinarySpec {
        bin: BIN,
        bind_var: "HA_BIND",
        required: &[("HA_HOST", &mock.uri()), ("HA_TOKEN", TOKEN)],
        extra: &[],
    })
    .await
    .unwrap();
}

/// `HA_INSECURE` from the environment reaches the TLS client, in every
/// spelling the README documents. It was parsed and dropped before, and only
/// `true` was parsed at all.
#[tokio::test]
async fn ha_insecure_from_env_is_honoured() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "message": "API running." })),
        )
        .mount(&mock)
        .await;
    let proxy = TlsProxy::start(*mock.address()).await.unwrap();
    let host = proxy.url();

    for (value, should_connect) in [
        (None, false),
        (Some("0"), false),
        (Some("1"), true),
        (Some("true"), true),
        (Some("yes"), true),
    ] {
        let bind = free_loopback_addr().unwrap();
        let mut envs = vec![
            ("HA_HOST", host.as_str()),
            ("HA_TOKEN", TOKEN),
            ("HA_BIND", bind.as_str()),
        ];
        if let Some(v) = value {
            envs.push(("HA_INSECURE", v));
        }
        let _server = ServerProcess::spawn(BIN, &bind, &envs).await.unwrap();
        let client = McpClient::connect(&format!("http://{bind}/mcp"))
            .await
            .unwrap();
        let res = client.call_tool("health_check", json!({})).await.unwrap();
        assert_eq!(
            res.is_success(),
            should_connect,
            "HA_INSECURE={value:?}: {:?}",
            res.message()
        );
    }
}
