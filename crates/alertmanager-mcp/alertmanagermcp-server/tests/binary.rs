//! The real `alertmanagermcp-server` executable: env → config → router →
//! socket.
//!
//! The write gate is an environment variable, so this is where it has to be
//! proven: only an affirmative `ALERTMANAGER_ALLOW_SILENCE` may register the
//! silence tools, and every other spelling (unset, empty, `0`, `false`, a typo)
//! must leave the server read-only.

use mcp_common::e2e::{
    BinarySpec, McpClient, ServerProcess, check_binary_contract, free_loopback_addr,
};
use wiremock::MockServer;

const BIN: &str = env!("CARGO_BIN_EXE_alertmanagermcp-server");

#[tokio::test]
async fn startup_contract() {
    let mock = MockServer::start().await;
    check_binary_contract(&BinarySpec {
        bin: BIN,
        bind_var: "ALERTMANAGER_BIND",
        required: &[("ALERTMANAGER_HOST", &mock.uri())],
        extra: &[],
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn only_an_affirmative_gate_value_registers_the_silence_tools() {
    let mock = MockServer::start().await;
    let uri = mock.uri();

    let mut observed = serde_json::Map::new();
    for gate in [
        None,
        Some(""),
        Some("0"),
        Some("false"),
        Some("ture"),
        Some("1"),
        Some("true"),
        Some("yes"),
    ] {
        let bind = free_loopback_addr().unwrap();
        let mut envs = vec![
            ("ALERTMANAGER_HOST", uri.as_str()),
            ("ALERTMANAGER_BIND", bind.as_str()),
        ];
        if let Some(v) = gate {
            envs.push(("ALERTMANAGER_ALLOW_SILENCE", v));
        }
        let _server = ServerProcess::spawn(BIN, &bind, &envs).await.unwrap();
        let client = McpClient::connect(&format!("http://{bind}/mcp"))
            .await
            .unwrap();
        let names = client.tool_names().await.unwrap();
        let writes: Vec<&String> = names
            .iter()
            .filter(|n| *n == "create_silence" || *n == "expire_silence")
            .collect();
        observed.insert(
            gate.map_or_else(|| "<unset>".to_string(), |v| format!("{v:?}")),
            serde_json::json!(writes),
        );
    }
    insta::assert_json_snapshot!("binary_gate_values", observed);
}
