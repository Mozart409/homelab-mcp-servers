//! End-to-end tests: the production router on a real socket, driven over MCP,
//! against a wiremock stand-in for the Woodpecker CI REST API.
//!
//! Failure modes this suite exists to catch (written before the tests):
//!
//! - `lookup_repo` encodes `owner/name` as one segment (`%2F`), or fails to
//!   encode reserved characters *inside* either part;
//! - Woodpecker's parameter spellings are lost: `per_page` must go out as
//!   `perPage` (and `ref` must reach it as `ref`), and unset filters must not be sent at all;
//! - `step_logs` returns base64 instead of text, or crashes on odd entries
//!   (`data` missing, `null`, or not base64). A single malformed line must not
//!   cost the whole log;
//! - truncation keeps the **wrong end** (the cause of a failure is at the
//!   end), miscounts `total_entries`, ignores an explicit `max_lines`, or
//!   treats `max_lines: 0` as "return nothing";
//! - `/healthz` answers 200 with an empty body, which must be a success
//!   (`null`), not a JSON parse error;
//! - Woodpecker's empty-bodied 404, a 401 with a text body, a 500, truncated
//!   JSON, or a dead host is not a tool error, or poisons the session;
//! - the bearer token is not sent;
//! - any tool sends anything but GET; a foreign `Host` is served.
//!
//! Artifacts: the contract, the method surface, and each scenario's
//! call → upstream → response transcript, under `tests/snapshots/`.

use color_eyre::eyre::Result;
use mcp_common::e2e::{self, McpClient, Scenario, TestServer};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wpmcp::Config;

const TOKEN: &str = "wp-pat-4c1d8e2f";

fn config(base_url: &str) -> Config {
    Config {
        base_url: base_url.to_string(),
        token: TOKEN.to_string(),
        insecure: false,
        bind: "127.0.0.1:0".to_string(),
        allowed_hosts: None,
        max_log_lines: 5,
    }
}

async fn serve(config: &Config) -> Result<(TestServer, McpClient)> {
    let server = TestServer::start(wpmcp::router(config)?).await?;
    let client = server.connect().await?;
    Ok((server, client))
}

async fn mount(mock: &MockServer, p: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(mock)
        .await;
}

/// A Woodpecker log entry as the API sends it: `data` is base64.
fn line(n: u64, text: &str) -> Value {
    use base64::Engine;
    json!({
        "id": 9000 + n, "step_id": 311, "time": n, "line": n, "type": 0,
        "data": base64::engine::general_purpose::STANDARD.encode(text)
    })
}

#[tokio::test]
async fn contract() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    insta::assert_json_snapshot!("contract", e2e::contract(&client).await.unwrap());
    e2e::check_doc_resource(
        &client,
        "doc://wpmcp/guide",
        include_str!("../../README.md"),
    )
    .await
    .unwrap();
    assert!(mock.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn every_tool_sends_only_get_requests() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    let surface = e2e::method_surface(&client, &mock, &json!([]))
        .await
        .unwrap();
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
        wpmcp::router(&cfg)
    })
    .await
    .unwrap();
}

/// "Why did the last push to main fail?" From repo lookup to the failing
/// step's log tail, the way a post-mortem runs.
#[tokio::test]
// One investigation, read top to bottom; splitting it would hide the story.
#[allow(clippy::too_many_lines)]
async fn failed_pipeline_postmortem() {
    let mock = MockServer::start().await;
    mount(
        &mock,
        "/api/repos/lookup/home%20lab/mcp%2Bservers",
        json!({ "id": 7 }),
    )
    .await;
    mount(&mock, "/api/repos/lookup/homelab/homelab-mcp-servers", json!({
        "id": 7, "forge_remote_id": "42", "owner": "homelab", "name": "homelab-mcp-servers",
        "full_name": "homelab/homelab-mcp-servers", "default_branch": "main", "active": true, "trusted": { "network": false, "security": false, "volumes": false }
    })).await;
    mount(
        &mock,
        "/api/repos/7",
        json!({ "id": 7, "full_name": "homelab/homelab-mcp-servers", "timeout": 60 }),
    )
    .await;
    mount(&mock, "/api/repos/7/pipelines", json!([
        { "id": 3301, "number": 212, "event": "push", "status": "failure", "branch": "main", "ref": "refs/heads/main",
          "commit": "4f30910", "message": "ci(nix): check containerfile toolchain pin", "author": "amadeus", "started": 1_727_300_000, "finished": 1_727_300_544 }
    ])).await;
    mount(
        &mock,
        "/api/repos/7/pipelines/212",
        json!({
            "number": 212, "status": "failure",
            "workflows": [{ "id": 610, "name": "test", "state": "failure", "children": [
                { "id": 311, "pid": 3, "name": "cargo test", "state": "failure", "exit_code": 101 },
                { "id": 312, "pid": 4, "name": "trivy", "state": "skipped", "exit_code": 0 }
            ]}]
        }),
    )
    .await;
    mount(&mock, "/api/repos/7/pipelines/212/config", json!([
        { "name": ".woodpecker/test.yaml", "hash": "a91c", "data": "c3RlcHM6CiAgLSBuYW1lOiBjYXJnbyB0ZXN0Cg==" }
    ])).await;
    mount(&mock, "/api/repos/7/pipelines/212/metadata", json!({ "repo": { "name": "homelab-mcp-servers" }, "curr": { "number": 212, "event": "push" } })).await;
    // 8 lines, a cap of 5: the last five must come back, decoded, including
    // the malformed ones, which must not break the rest.
    let mut entries: Vec<Value> = (1..=5)
        .map(|n| line(n, &format!("   Compiling crate-{n} v0.1.0")))
        .collect();
    entries.push(json!({ "id": 9006, "line": 6, "data": null }));
    entries.push(json!({ "id": 9007, "line": 7, "data": "%%% not base64 %%%" }));
    entries.push(line(
        8,
        "error: test failed, to rerun pass `-p pgmcp --test e2e`",
    ));
    mount(&mock, "/api/repos/7/logs/212/311", Value::Array(entries)).await;
    mount(
        &mock,
        "/api/repos/7/branches",
        json!(["main", "feat/e2e-test-harness"]),
    )
    .await;
    mount(
        &mock,
        "/api/repos/7/pull_requests",
        json!([{ "index": "18", "title": "feat: e2e test harness" }]),
    )
    .await;
    mount(
        &mock,
        "/api/repos/7/cron",
        json!([{ "id": 2, "name": "nightly", "schedule": "@daily", "branch": "main" }]),
    )
    .await;
    mount(&mock, "/api/agents", json!([{ "id": 1, "name": "homelab-agent-01", "platform": "linux/amd64", "capacity": 2, "last_contact": 1_727_300_600 }])).await;
    mount(
        &mock,
        "/api/agents/1/tasks",
        json!([{ "id": "3302", "pipeline_id": 3302, "dependencies": [] }]),
    )
    .await;
    mount(&mock, "/api/queue/info", json!({ "pending": null, "running": [{ "id": "3302" }], "stats": { "worker_count": 2, "pending_count": 0, "running_count": 1 }, "paused": false })).await;
    mount(
        &mock,
        "/api/user/repos",
        json!([{ "id": 7, "full_name": "homelab/homelab-mcp-servers" }]),
    )
    .await;
    mount(
        &mock,
        "/api/user/feed",
        json!([{ "repo_id": 7, "number": 212, "status": "failure" }]),
    )
    .await;
    mount(
        &mock,
        "/version",
        json!({ "source": "https://github.com/woodpecker-ci/woodpecker", "version": "3.9.0" }),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/healthz"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock)
        .await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    for (tool, args) in [
        ("version", json!({})),
        ("healthz", json!({})),
        ("list_repos", json!({ "all": true, "name": "mcp" })),
        (
            "lookup_repo",
            json!({ "owner": "home lab", "name": "mcp+servers" }),
        ),
        (
            "lookup_repo",
            json!({ "owner": "homelab", "name": "homelab-mcp-servers" }),
        ),
        ("get_repo", json!({ "repo_id": 7 })),
        (
            "list_pipelines",
            json!({ "repo_id": 7, "branch": "main", "event": "push", "status": "failure",
            "ref": "refs/heads/main", "before": "2024-09-26T00:00:00Z", "after": "2024-09-25T00:00:00Z", "page": 1, "per_page": 5 }),
        ),
        ("list_pipelines", json!({ "repo_id": 7 })),
        ("get_pipeline", json!({ "repo_id": 7, "number": 212 })),
        ("pipeline_config", json!({ "repo_id": 7, "number": 212 })),
        ("pipeline_metadata", json!({ "repo_id": 7, "number": 212 })),
        (
            "step_logs",
            json!({ "repo_id": 7, "number": 212, "step_id": 311 }),
        ),
        (
            "step_logs",
            json!({ "repo_id": 7, "number": 212, "step_id": 311, "max_lines": 20 }),
        ),
        (
            "step_logs",
            json!({ "repo_id": 7, "number": 212, "step_id": 311, "max_lines": 2 }),
        ),
        (
            "list_branches",
            json!({ "repo_id": 7, "page": 2, "per_page": 50 }),
        ),
        ("list_pull_requests", json!({ "repo_id": 7 })),
        ("list_crons", json!({ "repo_id": 7 })),
        ("list_agents", json!({ "per_page": 10 })),
        ("list_agent_tasks", json!({ "agent_id": 1 })),
        ("queue_info", json!({})),
        ("pipeline_feed", json!({})),
    ] {
        let res = s.call(tool, args.clone()).await.unwrap();
        assert!(res.is_success(), "{tool} {args}: {:?}", res.message());
    }

    // The default cap (5) keeps the LAST five: the failure line is in them.
    let default_cap = client
        .call_tool(
            "step_logs",
            json!({ "repo_id": 7, "number": 212, "step_id": 311 }),
        )
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(default_cap.get("total_entries"), Some(&json!(8)));
    assert_eq!(default_cap.get("returned_entries"), Some(&json!(5)));
    assert!(default_cap.to_string().contains("error: test failed"));

    insta::assert_json_snapshot!("failed_pipeline_postmortem", s.transcript());
}

/// `max_lines: 0` means "the server default", as `WP_MAX_LOG_LINES=0` does,
/// not "return no lines at all".
#[tokio::test]
async fn step_logs_max_lines_zero_means_the_default() {
    let mock = MockServer::start().await;
    let entries: Vec<Value> = (1..=8).map(|n| line(n, &format!("line {n}"))).collect();
    mount(&mock, "/api/repos/7/logs/212/311", Value::Array(entries)).await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    let logs = client
        .call_tool(
            "step_logs",
            json!({ "repo_id": 7, "number": 212, "step_id": 311, "max_lines": 0 }),
        )
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(logs.get("returned_entries"), Some(&json!(5)), "{logs}");
}

#[tokio::test]
async fn woodpecker_errors_are_tool_errors_and_the_session_survives() {
    let mock = MockServer::start().await;
    Mock::given(path("/api/repos/404"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock)
        .await;
    Mock::given(path("/api/repos/401"))
        .respond_with(ResponseTemplate::new(401).set_body_raw("token is expired\n", "text/plain"))
        .mount(&mock)
        .await;
    Mock::given(path("/api/repos/500"))
        .respond_with(ResponseTemplate::new(500).set_body_raw("database is locked", "text/plain"))
        .mount(&mock)
        .await;
    Mock::given(path("/api/repos/200"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(r#"{"id":200,"full_na"#, "application/json"),
        )
        .mount(&mock)
        .await;
    mount(&mock, "/version", json!({ "version": "3.9.0" })).await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    for id in [404, 401, 500, 200] {
        let res = s.call("get_repo", json!({ "repo_id": id })).await.unwrap();
        assert!(!res.is_success(), "repo {id} must fail");
        s.call("version", json!({})).await.unwrap().text().unwrap();
    }
    let dot = s
        .call("lookup_repo", json!({ "owner": "..", "name": "x" }))
        .await
        .unwrap();
    assert!(
        dot.error_message()
            .unwrap()
            .contains("not a valid path segment")
    );
    insta::assert_json_snapshot!("woodpecker_errors", s.transcript());
}

#[tokio::test]
async fn unreachable_woodpecker_is_a_prompt_tool_error() {
    let dead = format!("http://{}", e2e::free_loopback_addr().unwrap());
    let (_server, client) = serve(&config(&dead)).await.unwrap();

    let started = std::time::Instant::now();
    let msg = client
        .call_tool("version", json!({}))
        .await
        .unwrap()
        .error_message()
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(msg.contains("/version"), "{msg}");
}

#[tokio::test]
async fn insecure_flag_is_applied_both_ways() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": "3.9.0" })))
        .mount(&mock)
        .await;
    e2e::check_insecure_flag(
        &mock,
        |base_url, insecure| {
            let mut cfg = config(base_url);
            cfg.insecure = insecure;
            wpmcp::router(&cfg)
        },
        "version",
        json!({}),
    )
    .await
    .unwrap();
}
