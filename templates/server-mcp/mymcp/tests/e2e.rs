//! End-to-end tests: the production router on a real socket, driven over MCP,
//! against a wiremock stand-in for the <service> API.
//!
//! Failure modes this suite exists to catch. Write these first, then the tests,
//! then the code:
//!
//! - TODO: what the upstream API can say that a tool could misreport
//!   (error envelopes sent with 200, truncated JSON, HTML error pages);
//! - TODO: which argument bytes must survive encoding (queries, names, `..`);
//! - an optional filter leaks as an empty param when omitted;
//! - a dead host hangs or panics instead of failing the call, or poisons the
//!   session;
//! - any tool sends anything but GET; a foreign `Host` is served.
//!
//! Artifacts: the contract, the method surface, and each scenario's
//! call → upstream → response transcript, under `tests/snapshots/`.

use color_eyre::eyre::Result;
use mcp_common::e2e::{self, McpClient, Scenario, TestServer};
use mymcp::Config;
use serde_json::json;
use wiremock::MockServer;

fn config(base_url: &str) -> Config {
    Config {
        base_url: base_url.to_string(),
        bind: "127.0.0.1:0".to_string(),
        allowed_hosts: None,
        // TODO: every other field, with realistic values
    }
}

async fn serve(config: &Config) -> Result<(TestServer, McpClient)> {
    let server = TestServer::start(mymcp::router(config)?).await?;
    let client = server.connect().await?;
    Ok((server, client))
}

#[tokio::test]
async fn contract() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    insta::assert_json_snapshot!("contract", e2e::contract(&client).await.unwrap());
    e2e::check_doc_resource(&client, "doc://mymcp/guide", include_str!("../../README.md"))
        .await
        .unwrap();
    assert!(mock.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn every_tool_sends_only_get_requests() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    let surface = e2e::method_surface(&client, &mock, &json!({})).await.unwrap();
    insta::assert_json_snapshot!("method_surface", surface);
    for (tool, methods) in surface.as_object().unwrap() {
        assert_eq!(methods, &json!(["GET"]), "{tool} must only GET");
    }
}

#[tokio::test]
async fn dns_rebinding_guard() {
    let mock = MockServer::start().await;
    let uri = mock.uri();
    e2e::check_host_allow_list(|allowed_hosts| {
        let mut cfg = config(&uri);
        cfg.allowed_hosts = allowed_hosts;
        mymcp::router(&cfg)
    })
    .await
    .unwrap();
}

/// TODO: the question an operator actually asks, answered across several
/// tools in one session. Pick a medium-to-hard one, not the happy path.
#[tokio::test]
async fn investigation() {
    let mock = MockServer::start().await;
    // TODO: mount the upstream responses this investigation reads.
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    let mut s = Scenario::new(&client, &mock);
    // TODO: s.call("tool", json!({ … })).await.unwrap();
    insta::assert_json_snapshot!("investigation", s.transcript());
}
