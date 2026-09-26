//! End-to-end tests: the production router on a real socket, driven over MCP,
//! against a wiremock stand-in for the Alertmanager v2 API.
//!
//! alertmanager-mcp is one of the two servers allowed to write (AGENTS.md hard
//! rule §1, ADR 0001), and only behind `ALERTMANAGER_ALLOW_SILENCE`. So the
//! gate is the first thing verified, in both states, over the wire.
//!
//! Failure modes this suite exists to catch (written before the tests):
//!
//! - **the gate leaks**: with it closed, `create_silence`/`expire_silence`
//!   appear in `tools/list`, or can be *called* anyway (present-but-refusing
//!   is not enough, they must not be registered), or reach Alertmanager;
//! - **the write surface grows**: with the gate open, anything but those two
//!   tools sends a non-GET, or they send something other than one POST and
//!   one DELETE;
//! - a silence with **no matchers**, which suppresses every alert, is created
//!   by omitting an argument;
//! - the silence body is not what Alertmanager expects: camelCase keys,
//!   matcher defaults (`isRegex: false`, `isEqual: true`), `startsAt` kept
//!   when given;
//! - `filter` (repeated) and the boolean switches are mis-sent;
//! - the server's `instructions` misstate the write posture, so the LLM tells
//!   the operator it cannot silence when it can, or the reverse;
//! - Alertmanager's validation 400 (`end time must not be before start`), an
//!   empty-bodied 404, truncated JSON, or a dead host is not a tool error, or
//!   poisons the session;
//! - a silence ID of `..` turns `DELETE /api/v2/silence/{id}` into a DELETE
//!   on another path;
//! - a foreign `Host` is served.
//!
//! Artifacts: the contract and method surface **for both gate states**, and
//! each scenario's call → upstream → response transcript, under
//! `tests/snapshots/`.

use alertmanagermcp::Config;
use color_eyre::eyre::Result;
use mcp_common::e2e::{self, McpClient, Scenario, TestServer};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SILENCE_ID: &str = "8b1d64b3-5f2a-4c7e-9d0b-2e6f1a3c4d5e";

fn config(base_url: &str, allow_silence: bool) -> Config {
    Config {
        base_url: base_url.to_string(),
        token: None,
        insecure: false,
        bind: "127.0.0.1:0".to_string(),
        allowed_hosts: None,
        allow_silence,
    }
}

async fn serve(config: &Config) -> Result<(TestServer, McpClient)> {
    let server = TestServer::start(alertmanagermcp::router(config)?).await?;
    let client = server.connect().await?;
    Ok((server, client))
}

async fn mount(mock: &MockServer, verb: &str, p: &str, status: u16, body: Value) {
    Mock::given(method(verb))
        .and(path(p))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(mock)
        .await;
}

// ---- The gate ---------------------------------------------------------------

#[tokio::test]
async fn contract_with_the_gate_closed_and_open() {
    let mock = MockServer::start().await;
    for (label, allow) in [("closed", false), ("open", true)] {
        let (_server, client) = serve(&config(&mock.uri(), allow)).await.unwrap();
        insta::assert_json_snapshot!(
            format!("contract_gate_{label}"),
            e2e::contract(&client).await.unwrap()
        );
        e2e::check_doc_resource(
            &client,
            "doc://alertmanagermcp/guide",
            include_str!("../../README.md"),
        )
        .await
        .unwrap();
    }
    assert!(mock.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn gate_closed_write_tools_are_unregistered_and_uncallable() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri(), false)).await.unwrap();

    let names = client.tool_names().await.unwrap();
    assert!(
        !names
            .iter()
            .any(|n| n.contains("silence") && n != "list_silences" && n != "get_silence"),
        "write tools listed with the gate closed: {names:?}"
    );

    // Calling them by name anyway must fail at the protocol layer and never
    // reach Alertmanager.
    for (tool, args) in [
        (
            "create_silence",
            json!({ "matchers": [{ "name": "alertname", "value": "Watchdog" }],
            "ends_at": "2030-01-01T00:00:00Z", "created_by": "mallory", "comment": "x" }),
        ),
        ("expire_silence", json!({ "id": SILENCE_ID })),
    ] {
        let res = client.call_tool(tool, args).await.unwrap();
        assert!(
            !res.is_success(),
            "{tool} must not be callable with the gate closed"
        );
    }
    assert!(mock.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn method_surface_in_both_gate_states() {
    let mock = MockServer::start().await;
    let mut surfaces = serde_json::Map::new();
    for (label, allow) in [("closed", false), ("open", true)] {
        let (_server, client) = serve(&config(&mock.uri(), allow)).await.unwrap();
        surfaces.insert(
            label.into(),
            e2e::method_surface(&client, &mock, &json!([]))
                .await
                .unwrap(),
        );
    }
    insta::assert_json_snapshot!("method_surface", surfaces);

    let open = surfaces.get("open").and_then(Value::as_object).unwrap();
    for (tool, methods) in open {
        let expected = match tool.as_str() {
            "create_silence" => json!(["POST"]),
            "expire_silence" => json!(["DELETE"]),
            _ => json!(["GET"]),
        };
        assert_eq!(methods, &expected, "{tool}");
    }
}

#[tokio::test]
async fn dns_rebinding_guard() {
    let mock = MockServer::start().await;
    let uri = mock.uri();
    e2e::check_host_allow_list(|allowed_hosts| {
        let mut cfg = config(&uri, true);
        cfg.allowed_hosts = allowed_hosts;
        alertmanagermcp::router(&cfg)
    })
    .await
    .unwrap();
}

// ---- Scenarios --------------------------------------------------------------

/// "The backup job is being re-run by hand tonight; silence its alerts for
/// two hours, then end the silence early." Inspect, silence, verify, expire.
#[tokio::test]
async fn maintenance_window_silence_lifecycle() {
    let mock = MockServer::start().await;
    let alert = json!({
        "labels": { "alertname": "BackupJobFailed", "job": "pbs", "instance": "pbs01:8007", "severity": "critical" },
        "annotations": { "summary": "Nightly backup of vm/101 failed" },
        "startsAt": "2024-09-25T01:15:00.000Z", "endsAt": "2024-09-25T09:15:00.000Z",
        "fingerprint": "5e7d4c1b2a3f6e8d", "receivers": [{ "name": "ntfy-critical" }],
        "status": { "state": "active", "silencedBy": [], "inhibitedBy": [] }
    });
    mount(&mock, "GET", "/api/v2/alerts", 200, json!([alert])).await;
    mount(&mock, "GET", "/api/v2/alerts/groups", 200, json!([{
        "labels": { "alertname": "BackupJobFailed" }, "receiver": { "name": "ntfy-critical" }, "alerts": [alert]
    }])).await;
    mount(
        &mock,
        "GET",
        "/api/v2/receivers",
        200,
        json!([{ "name": "ntfy-critical" }, { "name": "null" }]),
    )
    .await;
    mount(&mock, "GET", "/api/v2/status", 200, json!({
        "cluster": { "status": "disabled", "peers": [] },
        "versionInfo": { "version": "0.28.1", "revision": "b2099ea", "goVersion": "go1.23.4" },
        "uptime": "2024-09-20T10:00:00.000Z", "config": { "original": "route:\n  receiver: ntfy-critical\n" }
    })).await;
    mount(
        &mock,
        "POST",
        "/api/v2/silences",
        200,
        json!({ "silenceID": SILENCE_ID }),
    )
    .await;
    mount(&mock, "GET", "/api/v2/silences", 200, json!([{
        "id": SILENCE_ID, "status": { "state": "active" },
        "matchers": [{ "name": "alertname", "value": "BackupJobFailed", "isRegex": false, "isEqual": true },
                     { "name": "instance", "value": "pbs0[12]:.*", "isRegex": true, "isEqual": true }],
        "startsAt": "2024-09-25T20:00:00.000Z", "endsAt": "2024-09-25T22:00:00.000Z",
        "createdBy": "amadeus", "comment": "manual re-run of vm/101 backup", "updatedAt": "2024-09-25T20:00:00.000Z"
    }])).await;
    mount(
        &mock,
        "GET",
        &format!("/api/v2/silence/{SILENCE_ID}"),
        200,
        json!({ "id": SILENCE_ID, "status": { "state": "active" } }),
    )
    .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v2/silence/{SILENCE_ID}")))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock)
        .await;

    let (_server, client) = serve(&config(&mock.uri(), true)).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    for (tool, args) in [
        ("status", json!({})),
        ("list_receivers", json!({})),
        (
            "list_alerts",
            json!({ "active": true, "silenced": false, "inhibited": false,
            "filter": ["alertname=\"BackupJobFailed\"", "severity=~\"critical|warning\""], "receiver": "ntfy-.*" }),
        ),
        ("alert_groups", json!({ "filter": ["job=\"pbs\""] })),
        (
            "create_silence",
            json!({
                "matchers": [
                    { "name": "alertname", "value": "BackupJobFailed" },
                    { "name": "instance", "value": "pbs0[12]:.*", "is_regex": true }
                ],
                "starts_at": "2024-09-25T20:00:00Z", "ends_at": "2024-09-25T22:00:00Z",
                "created_by": "amadeus", "comment": "manual re-run of vm/101 backup"
            }),
        ),
        (
            "list_silences",
            json!({ "filter": ["alertname=\"BackupJobFailed\""] }),
        ),
        ("get_silence", json!({ "id": SILENCE_ID })),
        ("expire_silence", json!({ "id": SILENCE_ID })),
    ] {
        let res = s.call(tool, args.clone()).await.unwrap();
        assert!(res.is_success(), "{tool} {args}: {:?}", res.message());
    }
    insta::assert_json_snapshot!("maintenance_window_silence_lifecycle", s.transcript());
}

/// The refusals: no matchers (would silence everything), Alertmanager's own
/// validation, an unknown silence, a dot-segment ID. None may be mistaken for
/// success, the matcher-less one must never reach Alertmanager, and the
/// session must survive each.
#[tokio::test]
async fn silence_refusals_and_upstream_errors() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/silences"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!("start time must be before end time")),
        )
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v2/silence/00000000-0000-0000-0000-000000000000"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/silences"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(r#"[{"id":"8b1d"#, "application/json"),
        )
        .mount(&mock)
        .await;
    mount(
        &mock,
        "GET",
        "/api/v2/receivers",
        200,
        json!([{ "name": "null" }]),
    )
    .await;

    let (_server, client) = serve(&config(&mock.uri(), true)).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    let silence = |matchers: Value, ends_at: &str| {
        json!({
            "matchers": matchers, "starts_at": "2024-09-25T22:00:00Z", "ends_at": ends_at,
            "created_by": "amadeus", "comment": "test"
        })
    };
    for (tool, args) in [
        ("create_silence", silence(json!([]), "2024-09-25T23:00:00Z")),
        (
            "create_silence",
            silence(
                json!([{ "name": "job", "value": "pbs" }]),
                "2024-09-25T21:00:00Z",
            ),
        ),
        (
            "expire_silence",
            json!({ "id": "00000000-0000-0000-0000-000000000000" }),
        ),
        ("expire_silence", json!({ "id": ".." })),
        ("list_silences", json!({})),
    ] {
        let res = s.call(tool, args.clone()).await.unwrap();
        assert!(!res.is_success(), "{tool} {args} must fail");
        s.call("list_receivers", json!({}))
            .await
            .unwrap()
            .text()
            .unwrap();
    }
    insta::assert_json_snapshot!("silence_refusals_and_upstream_errors", s.transcript());

    let transcript = s.transcript();
    let first = transcript.get(0).unwrap();
    assert_eq!(
        first.get("upstream"),
        Some(&json!([])),
        "matcher-less silence reached Alertmanager"
    );
}

#[tokio::test]
async fn unreachable_alertmanager_is_a_prompt_tool_error() {
    let dead = format!("http://{}", e2e::free_loopback_addr().unwrap());
    let (_server, client) = serve(&config(&dead, true)).await.unwrap();

    let started = std::time::Instant::now();
    let msg = client
        .call_tool("status", json!({}))
        .await
        .unwrap()
        .error_message()
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(msg.contains("/api/v2/status"), "{msg}");
}

#[tokio::test]
async fn insecure_flag_is_applied_both_ways() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/status"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "cluster": { "status": "disabled" } })),
        )
        .mount(&mock)
        .await;
    e2e::check_insecure_flag(
        &mock,
        |base_url, insecure| {
            let mut cfg = config(base_url, false);
            cfg.insecure = insecure;
            alertmanagermcp::router(&cfg)
        },
        "status",
        json!({}),
    )
    .await
    .unwrap();
}
