//! The real `mymcp-server` executable: env → config → router → socket.
//!
//! Anything read from the environment is proven here, against the built
//! binary in a hermetic env, not through `Config` in the library.

use mcp_common::e2e::{BinarySpec, check_binary_contract};
use wiremock::MockServer;

const BIN: &str = env!("CARGO_BIN_EXE_mymcp-server");

#[tokio::test]
async fn startup_contract() {
    let mock = MockServer::start().await;
    check_binary_contract(&BinarySpec {
        bin: BIN,
        bind_var: "MY_BIND",
        required: &[("MY_HOST", &mock.uri())],
        extra: &[],
    })
    .await
    .unwrap();
}
