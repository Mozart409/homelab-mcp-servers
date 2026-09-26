//! End-to-end tests: the production router on a real socket, driven over MCP,
//! against a wiremock stand-in for the Prometheus HTTP API.
//!
//! Failure modes this suite exists to catch (written before the tests):
//!
//! - `PromQL` loses meaning on the way to Prometheus: `{`, `}`, `"`, `=~`, `|`,
//!   `+` and spaces must survive query-string encoding byte for byte;
//! - `series` collapses several selectors into one `match[]`, or sends an
//!   empty window when none was given;
//! - an optional filter (`time`, `state`, `type`, `metric`) leaks as an empty
//!   param when omitted;
//! - Prometheus' own error envelope (`{"status":"error","errorType":…}`, sent
//!   with 400/422/503, **or with 200**) is reported as success, or loses the
//!   parse/execution message the LLM needs to fix its query;
//! - a non-JSON error page, truncated JSON, or a dead host hangs or panics
//!   instead of failing the call, or poisons the session;
//! - the bearer token is not sent;
//! - a label name of `..` escapes `/api/v1/label/{name}/values`;
//! - any tool sends anything but GET; a foreign `Host` is served.
//!
//! Artifacts: the contract, the method surface, and each scenario's
//! call → upstream → response transcript, under `tests/snapshots/`.

use color_eyre::eyre::Result;
use mcp_common::e2e::{self, McpClient, Scenario, TestServer};
use prommcp::Config;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "prom-reader-7f3a9c";

fn config(base_url: &str) -> Config {
    Config {
        base_url: base_url.to_string(),
        token: Some(TOKEN.to_string()),
        insecure: false,
        bind: "127.0.0.1:0".to_string(),
        allowed_hosts: None,
    }
}

async fn serve(config: &Config) -> Result<(TestServer, McpClient)> {
    let server = TestServer::start(prommcp::router(config)?).await?;
    let client = server.connect().await?;
    Ok((server, client))
}

fn success(data: &Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "status": "success", "data": data }))
}

async fn mount(mock: &MockServer, p: &str, data: Value) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(success(&data))
        .mount(mock)
        .await;
}

#[tokio::test]
async fn contract() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    insta::assert_json_snapshot!("contract", e2e::contract(&client).await.unwrap());
    e2e::check_doc_resource(
        &client,
        "doc://prommcp/guide",
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

    let surface = e2e::method_surface(&client, &mock, &json!({ "status": "success", "data": [] }))
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
        prommcp::router(&cfg)
    })
    .await
    .unwrap();
}

/// "The NAS disk alert is firing — how fast is it filling, and is the
/// exporter even healthy?" Every tool, the way an investigation uses them,
/// with `PromQL` full of characters that must survive encoding.
#[tokio::test]
async fn disk_filling_investigation() {
    let mock = MockServer::start().await;
    mount(&mock, "/api/v1/alerts", json!({ "alerts": [{
        "labels": { "alertname": "DiskWillFillIn24h", "instance": "nas-01:9100", "mountpoint": "/tank", "severity": "warning" },
        "annotations": { "summary": "/tank on nas-01 fills within 24h" },
        "state": "firing", "activeAt": "2024-09-25T03:12:44.117Z", "value": "8.123e+04"
    }]})).await;
    mount(&mock, "/api/v1/rules", json!({ "groups": [{
        "name": "node-disk", "file": "/etc/prometheus/rules/node.yml", "interval": 60,
        "rules": [{ "type": "alerting", "name": "DiskWillFillIn24h", "state": "firing", "health": "ok",
            "query": "predict_linear(node_filesystem_avail_bytes{fstype!~\"tmpfs|overlay\"}[6h], 86400) < 0",
            "duration": 1800, "labels": { "severity": "warning" } }]
    }]})).await;
    mount(&mock, "/api/v1/query", json!({ "resultType": "vector", "result": [
        { "metric": { "instance": "nas-01:9100", "mountpoint": "/tank" }, "value": [1_727_250_000.0, "0.0412"] }
    ]})).await;
    mount(&mock, "/api/v1/query_range", json!({ "resultType": "matrix", "result": [
        { "metric": { "instance": "nas-01:9100" }, "values": [[1_727_200_000, "0.061"], [1_727_225_000, "0.050"], [1_727_250_000, "0.041"]] }
    ]})).await;
    mount(&mock, "/api/v1/series", json!([
        { "__name__": "node_filesystem_avail_bytes", "instance": "nas-01:9100", "mountpoint": "/tank", "fstype": "zfs" },
        { "__name__": "node_filesystem_size_bytes", "instance": "nas-01:9100", "mountpoint": "/tank", "fstype": "zfs" }
    ])).await;
    mount(
        &mock,
        "/api/v1/labels",
        json!(["__name__", "fstype", "instance", "job", "mountpoint"]),
    )
    .await;
    mount(
        &mock,
        "/api/v1/label/__name__/values",
        json!([
            "node_filesystem_avail_bytes",
            "node_filesystem_size_bytes",
            "up"
        ]),
    )
    .await;
    mount(&mock, "/api/v1/targets", json!({ "activeTargets": [{
        "labels": { "instance": "nas-01:9100", "job": "node" }, "scrapeUrl": "http://nas-01:9100/metrics",
        "health": "down", "lastError": "Get \"http://nas-01:9100/metrics\": context deadline exceeded",
        "lastScrape": "2024-09-25T07:40:02.5Z", "lastScrapeDuration": 10.001
    }], "droppedTargets": [] })).await;
    mount(&mock, "/api/v1/metadata", json!({ "node_filesystem_avail_bytes": [
        { "type": "gauge", "help": "Filesystem space available to non-root users in bytes.", "unit": "" }
    ]})).await;
    mount(&mock, "/api/v1/status/tsdb", json!({
        "headStats": { "numSeries": 184_311, "numLabelPairs": 9_921, "chunkCount": 402_118, "minTime": 1_727_222_400_000_i64, "maxTime": 1_727_250_000_000_i64 },
        "seriesCountByMetricName": [{ "name": "node_systemd_unit_state", "value": 21_480 }]
    })).await;
    mount(&mock, "/api/v1/status/buildinfo", json!({
        "version": "3.1.0", "revision": "7c8ff2b", "branch": "HEAD", "buildDate": "20241201-10:02:11", "goVersion": "go1.23.4"
    })).await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    let promql = r#"1 - node_filesystem_avail_bytes{mountpoint=~"/tank|/", fstype!="tmpfs"} / node_filesystem_size_bytes"#;
    for (tool, args) in [
        ("alerts", json!({})),
        ("rules", json!({ "rule_type": "alert" })),
        ("query", json!({ "query": promql })),
        (
            "query",
            json!({ "query": promql, "time": "2024-09-25T07:40:00Z" }),
        ),
        (
            "query_range",
            json!({ "query": format!("sum by (instance) (rate({promql}[5m]))"),
            "start": "2024-09-24T07:40:00Z", "end": "2024-09-25T07:40:00Z", "step": "15m" }),
        ),
        (
            "series",
            json!({ "selectors": ["node_filesystem_avail_bytes{instance=\"nas-01:9100\"}", "node_filesystem_size_bytes"] }),
        ),
        (
            "series",
            json!({ "selectors": ["up"], "start": "1727222400", "end": "1727250000" }),
        ),
        ("labels", json!({})),
        ("label_values", json!({ "label": "__name__" })),
        ("targets", json!({ "state": "active" })),
        ("targets", json!({})),
        (
            "metadata",
            json!({ "metric": "node_filesystem_avail_bytes" }),
        ),
        ("tsdb_status", json!({})),
        ("build_info", json!({})),
    ] {
        s.call(tool, args).await.unwrap().text().unwrap();
    }
    insta::assert_json_snapshot!("disk_filling_investigation", s.transcript());
}

/// Every way Prometheus says "no", each followed by a healthy call on the same
/// session. The LLM must get Prometheus' own message (a parse error position,
/// "too many samples"), not a bare status code.
#[tokio::test]
async fn prometheus_errors_are_tool_errors_with_their_message() {
    let mock = MockServer::start().await;
    let err = |status: u16, kind: &str, msg: &str| {
        ResponseTemplate::new(status)
            .set_body_json(json!({ "status": "error", "errorType": kind, "error": msg }))
    };
    Mock::given(path("/api/v1/query"))
        .and(query_param("query", "rate(up[5m]"))
        .respond_with(err(
            400,
            "bad_data",
            "invalid parameter \"query\": 1:13: parse error: unclosed left parenthesis",
        ))
        .mount(&mock)
        .await;
    Mock::given(path("/api/v1/query"))
        .and(query_param("query", "count({__name__=~\".+\"}) by (job)"))
        .respond_with(err(
            422,
            "execution",
            "query processing would load too many samples into memory in query execution",
        ))
        .mount(&mock)
        .await;
    // Some proxies answer 200 with an error envelope; that is still an error.
    Mock::given(path("/api/v1/query"))
        .and(query_param("query", "vector(1)"))
        .respond_with(err(
            200,
            "timeout",
            "query timed out in expression evaluation",
        ))
        .mount(&mock)
        .await;
    Mock::given(path("/api/v1/query"))
        .and(query_param("query", "up"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_raw("Service Unavailable: TSDB still loading WAL", "text/plain"),
        )
        .mount(&mock)
        .await;
    Mock::given(path("/api/v1/query"))
        .and(query_param("query", "time()"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"status":"success","data":{"resultType":"sca"#,
            "application/json",
        ))
        .mount(&mock)
        .await;
    mount(&mock, "/api/v1/labels", json!(["job"])).await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    for q in [
        "rate(up[5m]",
        "count({__name__=~\".+\"}) by (job)",
        "vector(1)",
        "up",
        "time()",
    ] {
        let res = s.call("query", json!({ "query": q })).await.unwrap();
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
    insta::assert_json_snapshot!("prometheus_errors", s.transcript());
}

#[tokio::test]
async fn unreachable_prometheus_is_a_prompt_tool_error() {
    let dead = format!("http://{}", e2e::free_loopback_addr().unwrap());
    let (_server, client) = serve(&config(&dead)).await.unwrap();

    let started = std::time::Instant::now();
    let msg = client
        .call_tool("build_info", json!({}))
        .await
        .unwrap()
        .error_message()
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(msg.contains("/api/v1/status/buildinfo"), "{msg}");
}

#[tokio::test]
async fn insecure_flag_is_applied_both_ways() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/status/buildinfo"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "status": "success", "data": { "version": "3.1.0" } })),
        )
        .mount(&mock)
        .await;
    e2e::check_insecure_flag(
        &mock,
        |base_url, insecure| {
            let mut cfg = config(base_url);
            cfg.insecure = insecure;
            prommcp::router(&cfg)
        },
        "build_info",
        json!({}),
    )
    .await
    .unwrap();
}
