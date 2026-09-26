//! The real `pbsmcp-server` executable: env → config → router → socket.
//!
//! The in-process suite in `pbsmcp/tests/e2e.rs` builds `Config` by hand, so
//! this is the only place the `PBS_*` variables, `main`, and `--healthcheck`
//! are exercised as an operator's container runs them.

use mcp_common::e2e::{
    BinarySpec, McpClient, Scenario, ServerProcess, check_binary_contract, free_loopback_addr,
};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = env!("CARGO_BIN_EXE_pbsmcp-server");
const API_KEY: &str = "mcp@pbs!homelab:2f9c1d7e-5b8a-4c3f-9e21-7a6b5c4d3e2f";

#[tokio::test]
async fn startup_contract() {
    let mock = MockServer::start().await;
    check_binary_contract(&BinarySpec {
        bin: BIN,
        bind_var: "PBS_BIND",
        required: &[("PBS_HOST", &mock.uri()), ("PBS_API_KEY", API_KEY)],
        extra: &[],
    })
    .await
    .unwrap();
}

/// Optional settings read from the environment reach the wire: `PBS_NODE`
/// picks the node in task paths, and the key becomes a `PBSAPIToken` header.
#[tokio::test]
async fn env_settings_reach_the_upstream_requests() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api2/json/nodes/pbs-02/status"))
        .and(header("authorization", format!("PBSAPIToken={API_KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "uptime": 1_209_600, "cpu": 0.031, "memory": { "total": 16_777_216_000_u64, "used": 5_368_709_120_u64 } }
        })))
        .mount(&mock)
        .await;

    let bind = free_loopback_addr().unwrap();
    let uri = mock.uri();
    let _server = ServerProcess::spawn(
        BIN,
        &bind,
        &[
            ("PBS_HOST", &uri),
            ("PBS_API_KEY", API_KEY),
            ("PBS_NODE", "pbs-02"),
            ("PBS_BIND", &bind),
        ],
    )
    .await
    .unwrap();

    let client = McpClient::connect(&format!("http://{bind}/mcp"))
        .await
        .unwrap();
    let mut s = Scenario::new(&client, &mock);
    s.call("node_status", json!({}))
        .await
        .unwrap()
        .text()
        .unwrap();
    insta::assert_json_snapshot!("binary_node_status", s.transcript());
}
