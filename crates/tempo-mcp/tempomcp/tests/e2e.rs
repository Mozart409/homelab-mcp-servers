//! End-to-end tests: the production router on a real socket, driven over MCP,
//! against a wiremock stand-in for the Tempo HTTP API.
//!
//! Failure modes this suite exists to catch (written before the tests):
//!
//! - `TraceQL` loses meaning on the way to Tempo: braces, `&&`, quoted values,
//!   `duration > 2s`, pipelines like `| quantile_over_time(duration, .95) by (…)`;
//! - time arguments reach Tempo in a unit it misreads: RFC3339 sent verbatim
//!   (a 400), or milliseconds/nanoseconds sent as seconds (a silent empty
//!   window). RFC3339 with an offset must convert to the right instant;
//! - `search` is sent without a `limit`, so Tempo's server-side default
//!   decides how much lands in the LLM's context; or it returns spans;
//! - `trace` returns the raw OTLP document by default, or a tree whose span
//!   IDs are base64 (unusable in the next query), whose order is not a tree,
//!   which cuts spans without saying so, or whose cut hides the error and the
//!   slow span. An orphan span (parent never arrived) is dropped; a
//!   self-parented span or a parent cycle loops or loses spans; the older
//!   `batches` shape or integer enums parse as an empty trace;
//! - a trace ID of the wrong shape (base64, a path, `..`) or a tag name of
//!   `..` reaches the upstream path;
//! - a missing trace (404) is an unhelpful error or, worse, an empty success;
//! - Tempo's plain-text 400, a proxy's empty 401 or kilobytes of HTML 502,
//!   truncated JSON, or a dead host is not a tool error with a usable message,
//!   or poisons the session;
//! - the bearer token or tenant (`X-Scope-OrgID`) is not sent — without the
//!   tenant a multi-tenant Tempo answers "no traces", not an error;
//! - any tool sends anything but GET; a foreign `Host` is served.
//!
//! Artifacts: the contract, the method surface, and each scenario's
//! call → upstream → response transcript, under `tests/snapshots/`.

use color_eyre::eyre::Result;
use mcp_common::e2e::{self, McpClient, Scenario, TestServer};
use serde_json::{Value, json};
use tempomcp::Config;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The ID as Tempo's search returns it: leading zero trimmed.
const TRACE_ID: &str = "af7651916cd43dd8448eb211c80319c";

fn config(base_url: &str) -> Config {
    Config {
        base_url: base_url.to_string(),
        token: Some("otel-query-7f3a".to_string()),
        org_id: Some("homelab".to_string()),
        insecure: false,
        bind: "127.0.0.1:0".to_string(),
        allowed_hosts: None,
    }
}

async fn serve(config: &Config) -> Result<(TestServer, McpClient)> {
    let server = TestServer::start(tempomcp::router(config)?).await?;
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

async fn mount_text(mock: &MockServer, p: &str, status: u16, body: &str, content_type: &str) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(status).set_body_raw(body.to_string(), content_type))
        .mount(mock)
        .await;
}

async fn mount_status(mock: &MockServer) {
    mount_text(mock, "/api/echo", 200, "echo", "text/plain").await;
    mount_text(mock, "/ready", 200, "ready\n", "text/plain").await;
    mount(
        mock,
        "/api/status/buildinfo",
        json!({ "version": "2.8.1", "revision": "a1f9b3c", "branch": "HEAD", "goVersion": "go1.24.2" }),
    )
    .await;
}

#[tokio::test]
async fn contract() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    insta::assert_json_snapshot!("contract", e2e::contract(&client).await.unwrap());
    e2e::check_doc_resource(
        &client,
        "doc://tempomcp/guide",
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

    let surface = e2e::method_surface(&client, &mock, &json!({}))
        .await
        .unwrap();
    insta::assert_json_snapshot!("method_surface", surface);
    for (tool, methods) in surface.as_object().unwrap() {
        // `trace` refuses the harness's placeholder ID (`"sample"` is not hex)
        // before sending anything, so it is proven with a real ID below.
        if tool == "trace" {
            assert_eq!(methods, &json!([]), "trace must refuse a non-hex ID");
        } else {
            assert_eq!(methods, &json!(["GET"]), "{tool} must only GET");
        }
    }

    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&mock)
        .await;
    for args in [
        json!({ "trace_id": TRACE_ID }),
        json!({ "trace_id": TRACE_ID, "raw": true }),
    ] {
        client
            .call_tool("trace", args)
            .await
            .unwrap()
            .text()
            .unwrap();
    }
    assert_eq!(e2e::upstream_methods(&mock).await.unwrap(), ["GET"]);
}

#[tokio::test]
async fn dns_rebinding_guard() {
    let mock = MockServer::start().await;
    let uri = mock.uri();
    e2e::check_host_allow_list(|allowed_hosts| {
        let mut cfg = config(&uri);
        cfg.allowed_hosts = allowed_hosts;
        tempomcp::router(&cfg)
    })
    .await
    .unwrap();
}

/// The upstream half of [`slow_download_investigation`]: Tempo as it looks
/// shortly after the slow downloads, with `trace` as the one worth reading.
async fn mount_investigation(mock: &MockServer, trace: Value) {
    mount_status(mock).await;
    mount(
        mock,
        "/api/v2/search/tag/resource.service.name/values",
        json!({ "tagValues": [
            { "type": "string", "value": "garage" },
            { "type": "string", "value": "hofvarpnir" }
        ] }),
    )
    .await;
    mount(
        mock,
        "/api/v2/search/tags",
        json!({ "scopes": [
            { "name": "span", "tags": ["db.statement", "db.system", "http.route", "url.full"] }
        ] }),
    )
    .await;
    mount(mock, "/api/search", json!({
        "traces": [
            {
                "traceID": TRACE_ID,
                "rootServiceName": "hofvarpnir",
                "rootTraceName": "GET /api/download/{id}",
                "startTimeUnixNano": "1727230441000000000",
                "durationMs": 2350,
                "spanSets": [{
                    "spans": [{
                        "spanID": "1a2b3c4d5e6f7081",
                        "startTimeUnixNano": "1727230441000000000",
                        "durationNanos": "2350000000",
                        "attributes": [{ "key": "http.route", "value": { "stringValue": "/api/download/{id}" } }]
                    }],
                    "matched": 1
                }],
                "serviceStats": {
                    "garage": { "spanCount": 5, "errorCount": 1 },
                    "hofvarpnir": { "spanCount": 5, "errorCount": 1 }
                }
            },
            {
                "traceID": "5b8efff798038103d269b633813fc60c",
                "rootServiceName": "hofvarpnir",
                "rootTraceName": "GET /api/download/{id}",
                "startTimeUnixNano": "1727231012345678901",
                "durationMs": 2104,
                "spanSet": { "spans": [{ "spanID": "e1f2a3b4c5d6e7f8", "durationNanos": "2104000000" }], "matched": 1 }
            }
        ],
        "metrics": { "inspectedTraces": 412, "inspectedBytes": "1048576", "completedJobs": 12, "totalJobs": 12 }
    }))
    .await;
    mount(mock, &format!("/api/v2/traces/{TRACE_ID}"), trace).await;
    mount(
        mock,
        "/api/metrics/query_range",
        json!({ "series": [{
            "labels": [{ "key": "span.http.route", "value": { "stringValue": "/api/download/{id}" } }],
            "samples": [
                { "timestampMs": "1727229600000", "value": 0.41 },
                { "timestampMs": "1727229900000", "value": 2.35 }
            ]
        }] }),
    )
    .await;
}

/// "hofvarpnir downloads were slow around 02:15 — which span?" Is Tempo up,
/// is the service name real, what can be filtered on, which traces were slow,
/// and then the one that matters read three ways: the compact tree cut short
/// with a few attributes, the raw document in a window, and a `TraceQL`
/// metrics query for the p95 by route.
#[tokio::test]
async fn slow_download_investigation() {
    let mock = MockServer::start().await;
    let trace: Value = serde_json::from_str(include_str!("fixtures/download_trace.json")).unwrap();
    mount_investigation(&mock, trace).await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    let slow = r#"{ resource.service.name = "hofvarpnir" && kind = server && duration > 2s }"#;
    for (tool, args) in [
        ("status", json!({})),
        (
            "search_tag_values",
            json!({ "tag": "resource.service.name" }),
        ),
        (
            "search_tags",
            json!({ "scope": "span", "q": r#"{ resource.service.name = "hofvarpnir" }"#, "start": "2024-09-25T02:00:00Z" }),
        ),
        (
            "search",
            json!({ "q": slow, "start": "2024-09-25T02:00:00Z", "end": "2024-09-25T04:30:00+02:00" }),
        ),
        (
            "trace",
            json!({ "trace_id": TRACE_ID, "max_spans": 3, "attributes": ["http.route", "url.full", "db.statement", "service.version"] }),
        ),
        (
            "trace",
            json!({ "trace_id": TRACE_ID.to_uppercase(), "start": "1727229600", "end": "1727233200", "raw": true }),
        ),
        (
            "metrics_query_range",
            json!({ "q": r#"{ resource.service.name = "hofvarpnir" } | quantile_over_time(duration, .95) by (span.http.route)"#, "start": "2024-09-25T02:00:00Z", "end": "2024-09-25T02:30:00Z", "step": "5m" }),
        ),
    ] {
        s.call(tool, args).await.unwrap().text().unwrap();
    }

    let steps = s.transcript();
    let tree = steps.pointer("/4/response/ok").unwrap();
    // The cut is announced, and the answer survives it: garage's failed
    // `GetObject` and the 1.48s disk read beneath it both sit past span 3 in
    // tree order, yet appear in `errors` and `slowest`.
    assert_eq!(tree["truncated"], json!(true));
    let listed: Vec<&Value> = tree["spans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| &s["spanID"])
        .collect();
    assert_eq!(listed.len(), 3);
    for (summary, id) in [
        ("errors", "4d5e6f708192a3b4"),
        ("slowest", "5e6f708192a3b4c5"),
    ] {
        assert!(!listed.contains(&&json!(id)), "{id} should be past the cut");
        assert!(
            tree[summary]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["spanID"] == id),
            "{id} missing from {summary}"
        );
    }
    assert_eq!(tree["errorCount"], json!(2));
    // IDs are hex, not Tempo's base64.
    assert_eq!(
        tree.pointer("/spans/0/spanID"),
        Some(&json!("1a2b3c4d5e6f7081"))
    );
    insta::assert_json_snapshot!("slow_download_investigation", steps);
}

/// Every shape Tempo (or a misbehaving SDK) can hand the tree builder that is
/// not the happy path: the older `batches`/`instrumentationLibrarySpans`
/// layout with integer enums and hex IDs, a self-parented span, a two-span
/// parent cycle, an all-zero parent ID, a `PARTIAL` status, and an empty trace.
#[tokio::test]
async fn trace_shapes_that_are_not_the_happy_path() {
    let mock = MockServer::start().await;
    let span = |id: &str, parent: &str, name: &str, start: u64, kind: u64, code: u64| {
        json!({
            "spanId": id, "parentSpanId": parent, "name": name, "kind": kind,
            "startTimeUnixNano": 1_727_230_441_000_000_000_u64 + start * 1_000_000,
            "endTimeUnixNano": 1_727_230_441_000_000_000_u64 + (start + 10) * 1_000_000,
            "status": { "code": code }
        })
    };
    mount(
        &mock,
        "/api/v2/traces/1",
        json!({
            "status": "PARTIAL",
            "message": "trace exceeds max size (max bytes: 5000000), partial trace returned",
            "batches": [{
                "resource": { "attributes": [{ "key": "service.name", "value": { "stringValue": "wpmcp" } }] },
                "instrumentationLibrarySpans": [{ "spans": [
                    span("00000000000000a1", "0000000000000000", "root", 0, 2, 0),
                    span("00000000000000b2", "00000000000000b2", "self-parented", 5, 1, 2),
                    span("00000000000000c3", "00000000000000d4", "cycle-a", 7, 3, 1),
                    span("00000000000000d4", "00000000000000c3", "cycle-b", 8, 3, 0),
                    span("00000000000000e5", "00000000000000a1", "child", 1, 5, 0)
                ]}]
            }]
        }),
    )
    .await;
    mount(&mock, "/api/v2/traces/2", json!({ "trace": {} })).await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    for id in ["1", "2"] {
        s.call("trace", json!({ "trace_id": id }))
            .await
            .unwrap()
            .text()
            .unwrap();
    }

    let steps = s.transcript();
    let tree = steps.pointer("/0/response/ok").unwrap();
    let mut ids: Vec<&str> = tree["spans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["spanID"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    // Every span exactly once, cycle and self-parent included.
    assert_eq!(
        ids,
        [
            "00000000000000a1",
            "00000000000000b2",
            "00000000000000c3",
            "00000000000000d4",
            "00000000000000e5"
        ]
    );
    insta::assert_json_snapshot!("trace_shapes", steps);
}

/// Tempo's and its proxy's failure shapes, each a tool error with a usable
/// message, and the arguments that must be refused before any request —
/// with the session still answering at the end.
#[tokio::test]
async fn tempo_errors_and_refused_arguments() {
    let mock = MockServer::start().await;
    mount_status(&mock).await;
    Mock::given(path("/api/search"))
        .and(query_param("q", "{ resource.service.name = hofvarpnir }"))
        .respond_with(ResponseTemplate::new(400).set_body_raw(
            "invalid TraceQL query: parse error at line 1, col 27: syntax error: unexpected IDENTIFIER\n",
            "text/plain; charset=utf-8",
        ))
        .mount(&mock)
        .await;
    // Caddy's forward-auth refusal: no body at all.
    mount_text(&mock, "/api/v2/search/tags", 401, "", "text/plain").await;
    mount_text(
        &mock,
        "/api/v2/traces/deadbeef",
        404,
        "trace not found\n",
        "text/plain",
    )
    .await;
    let html = format!(
        "<!DOCTYPE html><html><head><title>502 Bad Gateway</title></head><body>{}</body></html>",
        "<p>upstream tempo-query-frontend:3200 is unreachable</p>".repeat(60)
    );
    mount_text(
        &mock,
        "/api/v2/search/tag/span.http.route/values",
        502,
        &html,
        "text/html",
    )
    .await;
    mount_text(
        &mock,
        "/api/metrics/query_range",
        200,
        r#"{"series":[{"labels":[{"key""#,
        "application/json",
    )
    .await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    let failing = [
        (
            "search",
            json!({ "q": "{ resource.service.name = hofvarpnir }" }),
        ),
        ("search_tags", json!({})),
        ("trace", json!({ "trace_id": "deadbeef" })),
        (
            "trace",
            json!({ "trace_id": "deadbeef", "start": "2024-09-25T02:00:00Z", "end": "2024-09-25T03:00:00Z" }),
        ),
        ("search_tag_values", json!({ "tag": "span.http.route" })),
        ("metrics_query_range", json!({ "q": "{} | rate()" })),
    ];
    let refused = [
        ("trace", json!({ "trace_id": "CvdlGRbNQ92ESOshHIAxnA==" })),
        ("trace", json!({ "trace_id": "../../api/echo" })),
        ("trace", json!({ "trace_id": "" })),
        ("search_tag_values", json!({ "tag": ".." })),
        ("search", json!({ "q": "{}", "start": "1727230441000" })),
        ("search", json!({ "q": "{}", "end": "1727230441000000000" })),
        ("search", json!({ "q": "{}", "start": "yesterday" })),
        (
            "search",
            json!({ "q": "{}", "start": "2024-09-25T03:00:00Z", "end": "2024-09-25T02:00:00Z" }),
        ),
        ("search_tags", json!({ "scope": "everything" })),
    ];
    let (failing_count, refused_count) = (failing.len(), refused.len());
    for (tool, args) in failing.into_iter().chain(refused) {
        let res = s.call(tool, args.clone()).await.unwrap();
        assert!(!res.is_success(), "{tool} {args} must fail");
    }
    s.call("status", json!({})).await.unwrap().text().unwrap();

    let steps = s.transcript();
    let steps = steps.as_array().unwrap();
    for step in steps.iter().skip(failing_count).take(refused_count) {
        assert_eq!(
            step["upstream"],
            json!([]),
            "refused before sending: {step}"
        );
    }
    let bad_gateway = steps.get(4).unwrap()["response"].to_string();
    assert!(
        bad_gateway.len() < 1000,
        "502 body is not cut: {bad_gateway}"
    );
    insta::assert_json_snapshot!("tempo_errors", steps);
}

#[tokio::test]
async fn unreachable_tempo_is_a_prompt_tool_error() {
    let dead = format!("http://{}", e2e::free_loopback_addr().unwrap());
    let (_server, client) = serve(&config(&dead)).await.unwrap();

    let started = std::time::Instant::now();
    for (tool, args, route) in [
        ("status", json!({}), "/api/echo"),
        ("search", json!({ "q": "{}" }), "/api/search"),
    ] {
        let msg = client
            .call_tool(tool, args)
            .await
            .unwrap()
            .error_message()
            .unwrap();
        assert!(msg.contains(route), "{msg}");
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[tokio::test]
async fn insecure_flag_is_applied_both_ways() {
    let mock = MockServer::start().await;
    mount(&mock, "/api/v2/search/tags", json!({ "scopes": [] })).await;
    e2e::check_insecure_flag(
        &mock,
        |base_url, insecure| {
            let mut cfg = config(base_url);
            cfg.insecure = insecure;
            tempomcp::router(&cfg)
        },
        "search_tags",
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
