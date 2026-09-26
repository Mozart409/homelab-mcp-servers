//! End-to-end tests: the production router on a real socket, driven over MCP,
//! against a wiremock stand-in for the Loki HTTP API.
//!
//! Failure modes this suite exists to catch (written before the tests):
//!
//! - `LogQL` loses meaning on the way to Loki: stream selectors, `|=`, `!=`,
//!   `|~` with backticked regexes, `| json`, `| line_format "{{.msg}}"`;
//! - `limit` is not defaulted to 100 when omitted, so a broad query returns
//!   Loki's server-side maximum into the LLM's context;
//! - optional `time`/`start`/`end`/`step`/`direction` leak as empty params;
//! - `series` collapses several selectors into one `match[]`;
//! - the tenant (`X-Scope-OrgID`) or bearer token is not sent. Without the
//!   tenant, a multi-tenant Loki answers with *another tenant's* nothing,
//!   which looks like "no logs" rather than an error;
//! - Loki's **plain-text** 400 (`parse error at line 1, col 12: …`) loses its
//!   message, or a 200 with `status: error`, truncated JSON, or a dead host is
//!   not a tool error, or poisons the session;
//! - `index_stats` (which Loki does not wrap in `data`) is mangled;
//! - a label name of `..` escapes its path segment;
//! - any tool sends anything but GET; a foreign `Host` is served.
//!
//! Artifacts: the contract, the method surface, and each scenario's
//! call → upstream → response transcript, under `tests/snapshots/`.

use color_eyre::eyre::Result;
use lokimcp::Config;
use mcp_common::e2e::{self, McpClient, Scenario, TestServer};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(base_url: &str) -> Config {
    Config {
        base_url: base_url.to_string(),
        token: Some("loki-reader-51c0".to_string()),
        org_id: Some("homelab".to_string()),
        insecure: false,
        bind: "127.0.0.1:0".to_string(),
        allowed_hosts: None,
    }
}

async fn serve(config: &Config) -> Result<(TestServer, McpClient)> {
    let server = TestServer::start(lokimcp::router(config)?).await?;
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

fn success(data: &Value) -> Value {
    json!({ "status": "success", "data": data })
}

#[tokio::test]
async fn contract() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    insta::assert_json_snapshot!("contract", e2e::contract(&client).await.unwrap());
    e2e::check_doc_resource(
        &client,
        "doc://lokimcp/guide",
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

    let surface = e2e::method_surface(&client, &mock, &success(&json!([])))
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
        lokimcp::router(&cfg)
    })
    .await
    .unwrap();
}

/// "Gitea returned 502s overnight — what did it log?" Label discovery first,
/// then a `LogQL` pipeline with every operator that is easy to mangle, then a
/// metric query over a window, then how much data the selector touches.
#[tokio::test]
async fn overnight_502_investigation() {
    let mock = MockServer::start().await;
    mount(
        &mock,
        "/loki/api/v1/labels",
        success(&json!([
            "container",
            "host",
            "job",
            "level",
            "service_name"
        ])),
    )
    .await;
    mount(
        &mock,
        "/loki/api/v1/label/service_name/values",
        success(&json!(["caddy", "gitea", "postgres"])),
    )
    .await;
    mount(
        &mock,
        "/loki/api/v1/series",
        success(&json!([
            { "service_name": "gitea", "host": "forge-01", "level": "error" },
            { "service_name": "caddy", "host": "edge-01", "level": "info" }
        ])),
    )
    .await;
    mount(&mock, "/loki/api/v1/query_range", success(&json!({
        "resultType": "streams",
        "result": [{
            "stream": { "service_name": "gitea", "host": "forge-01", "level": "error" },
            "values": [
                ["1727229541000000000", "{\"level\":\"error\",\"msg\":\"dial tcp 10.0.20.5:5432: connect: connection refused\",\"status\":502}"],
                ["1727229602000000000", "{\"level\":\"error\",\"msg\":\"pq: the database system is starting up\",\"status\":502}"]
            ]
        }],
        "stats": { "summary": { "bytesProcessedPerSecond": 812_392, "totalEntriesReturned": 2 } }
    }))).await;
    mount(
        &mock,
        "/loki/api/v1/query",
        success(&json!({
            "resultType": "vector",
            "result": [{ "metric": { "host": "forge-01" }, "value": [1_727_250_000, "37"] }]
        })),
    )
    .await;
    // Loki returns index stats bare, without the status/data envelope.
    mount(
        &mock,
        "/loki/api/v1/index/stats",
        json!({ "streams": 4, "chunks": 212, "bytes": 18_874_368, "entries": 91_422 }),
    )
    .await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    let pipeline = r#"{service_name="gitea", host=~"forge-.*"} |= "502" != "healthcheck" |~ `(?i)conn(ection)? refused|starting up` | json | line_format "{{.msg}}""#;
    for (tool, args) in [
        ("labels", json!({})),
        (
            "labels",
            json!({ "start": "2024-09-25T00:00:00Z", "end": "2024-09-25T08:00:00Z" }),
        ),
        ("label_values", json!({ "label": "service_name" })),
        (
            "series",
            json!({ "selectors": ["{service_name=\"gitea\"}", "{job=~\"caddy|traefik\"}"], "start": "2024-09-25T00:00:00Z" }),
        ),
        (
            "query_range",
            json!({ "query": pipeline, "start": "2024-09-25T00:00:00Z", "end": "2024-09-25T08:00:00Z", "direction": "backward" }),
        ),
        (
            "query_range",
            json!({ "query": pipeline, "limit": 5000, "step": "5m" }),
        ),
        (
            "query",
            json!({ "query": r#"sum by (host) (count_over_time({service_name="gitea"} |= "502" [8h]))"#, "time": "2024-09-25T08:00:00Z" }),
        ),
        (
            "query",
            json!({ "query": r#"{service_name="gitea"}"#, "direction": "forward", "limit": 3 }),
        ),
        (
            "index_stats",
            json!({ "query": r#"{service_name="gitea"}"#, "start": "2024-09-25T00:00:00Z", "end": "2024-09-25T08:00:00Z" }),
        ),
    ] {
        s.call(tool, args).await.unwrap().text().unwrap();
    }
    insta::assert_json_snapshot!("overnight_502_investigation", s.transcript());
}

#[tokio::test]
async fn loki_errors_are_tool_errors_with_their_message() {
    let mock = MockServer::start().await;
    // Loki's query errors are plain text, not JSON.
    Mock::given(path("/loki/api/v1/query_range"))
        .and(query_param("query", "{service_name=\"gitea\"} |= "))
        .respond_with(ResponseTemplate::new(400).set_body_raw(
            "parse error at line 1, col 26: syntax error: unexpected $end, expecting STRING\n",
            "text/plain; charset=utf-8",
        ))
        .mount(&mock)
        .await;
    Mock::given(path("/loki/api/v1/query_range"))
        .and(query_param("query", "{job=~\".+\"}"))
        .respond_with(ResponseTemplate::new(400).set_body_raw(
            "the query time range exceeds the limit (query length: 2160h0m0s, limit: 721h0m0s)\n",
            "text/plain; charset=utf-8",
        ))
        .mount(&mock)
        .await;
    Mock::given(path("/loki/api/v1/query_range"))
        .and(query_param("query", "{job=\"x\"}"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "status": "error", "error": "context canceled" })),
        )
        .mount(&mock)
        .await;
    Mock::given(path("/loki/api/v1/query_range"))
        .and(query_param("query", "{job=\"y\"}"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(r#"{"status":"success","data":{"result"#, "application/json"),
        )
        .mount(&mock)
        .await;
    mount(&mock, "/loki/api/v1/labels", success(&json!(["job"]))).await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    for q in [
        "{service_name=\"gitea\"} |= ",
        "{job=~\".+\"}",
        "{job=\"x\"}",
        "{job=\"y\"}",
    ] {
        let res = s.call("query_range", json!({ "query": q })).await.unwrap();
        assert!(!res.is_success(), "{q} must fail");
        s.call("labels", json!({})).await.unwrap().text().unwrap();
    }
    let dot = s
        .call("label_values", json!({ "label": ".." }))
        .await
        .unwrap();
    assert!(
        dot.error_message()
            .unwrap()
            .contains("not a valid path segment")
    );
    insta::assert_json_snapshot!("loki_errors", s.transcript());
}

#[tokio::test]
async fn unreachable_loki_is_a_prompt_tool_error() {
    let dead = format!("http://{}", e2e::free_loopback_addr().unwrap());
    let (_server, client) = serve(&config(&dead)).await.unwrap();

    let started = std::time::Instant::now();
    let msg = client
        .call_tool("labels", json!({}))
        .await
        .unwrap()
        .error_message()
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(msg.contains("/loki/api/v1/labels"), "{msg}");
}

#[tokio::test]
async fn insecure_flag_is_applied_both_ways() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/labels"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "status": "success", "data": [] })),
        )
        .mount(&mock)
        .await;
    e2e::check_insecure_flag(
        &mock,
        |base_url, insecure| {
            let mut cfg = config(base_url);
            cfg.insecure = insecure;
            lokimcp::router(&cfg)
        },
        "labels",
        json!({}),
    )
    .await
    .unwrap();
}

/// A misspelt argument is refused before anything is sent upstream, instead
/// of being dropped so that the call quietly answers a different question.
#[tokio::test]
async fn unknown_arguments_are_refused() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    let refusals = e2e::unknown_arguments(&client, Some(&mock)).await.unwrap();
    insta::assert_json_snapshot!("unknown_arguments", refusals);
}
