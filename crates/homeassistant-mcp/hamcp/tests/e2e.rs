//! End-to-end tests: the production router on a real socket, driven over MCP,
//! against a wiremock stand-in for the Home Assistant REST API.
//!
//! hamcp is the one server whose purpose is to change things (AGENTS.md hard
//! rule §1, exception 1), ungated. That shifts the worst case: for a read-only
//! server it is a wrong answer; here it is **a device command that failed but
//! was reported as done**, or one that went somewhere other than intended.
//!
//! Failure modes this suite exists to catch (written before the tests):
//!
//! - **the write surface drifts**: anything beyond `set_state`,
//!   `call_service`, and the two POST-only reads (`render_template`,
//!   `check_config`) sends a non-GET;
//! - **a failed write reads as success**: a 500 from `call_service` becomes an
//!   empty "no states changed", or a 400 loses its hint;
//! - **a command goes to the wrong target**: an entity ID with `/` or `..`
//!   changes the path (`set_state("..")` used to POST to `/api/`), or a
//!   top-level `entity_id` loses to one inside `service_data`, or
//!   `service_data` is dropped;
//! - the body shapes Home Assistant expects drift: `entity_id` omitted (not
//!   `null`) when not given; `set_state` with its attributes;
//! - history and calendar queries are mis-built: the start time belongs in the
//!   **path** (encoded), `filter_entity_id` is comma-joined, the flags are
//!   valueless query params;
//! - `render_template` returns HA's raw text, not a JSON-quoted string;
//! - a 401, an unknown entity (404), an HTML error page, truncated JSON, or a
//!   dead host is not a tool error, or poisons the session;
//! - `HA_INSECURE` is not applied (it was not, until this suite);
//! - a foreign `Host` is served.
//!
//! Artifacts: the contract, the method surface, and each scenario's
//! call → upstream → response transcript, under `tests/snapshots/`.

use color_eyre::eyre::Result;
use hamcp::Config;
use mcp_common::e2e::{self, McpClient, Scenario, TestServer};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.hamcp-e2e.c2lnbmF0dXJl";

fn config(base_url: &str) -> Config {
    Config {
        base_url: base_url.to_string(),
        token: TOKEN.to_string(),
        insecure: false,
        bind: "127.0.0.1:0".to_string(),
        allowed_hosts: None,
    }
}

async fn serve(config: &Config) -> Result<(TestServer, McpClient)> {
    let server = TestServer::start(hamcp::router(config)?).await?;
    let client = server.connect().await?;
    Ok((server, client))
}

async fn mount(mock: &MockServer, verb: &str, p: &str, template: ResponseTemplate) {
    Mock::given(method(verb))
        .and(path(p))
        .respond_with(template)
        .mount(mock)
        .await;
}

fn ok(body: &Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

fn light(state: &str, brightness: Option<u8>) -> Value {
    json!({
        "entity_id": "light.living_room",
        "state": state,
        "attributes": { "friendly_name": "Living Room", "brightness": brightness, "supported_color_modes": ["brightness"] },
        "last_changed": "2024-09-25T18:02:11.482930+00:00",
        "last_updated": "2024-09-25T18:02:11.482930+00:00",
        "context": { "id": "01J8Q5W7Z1K9R3T4V6X8Y0A2B4", "parent_id": null, "user_id": null }
    })
}

#[tokio::test]
async fn contract() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    insta::assert_json_snapshot!("contract", e2e::contract(&client).await.unwrap());
    e2e::check_doc_resource(
        &client,
        "doc://hamcp/guide",
        include_str!("../../README.md"),
    )
    .await
    .unwrap();
    assert!(mock.received_requests().await.unwrap().is_empty());
}

/// The write inventory. Changing it is a policy change (hard rule §1) and
/// must show up as a snapshot diff in review.
#[tokio::test]
async fn method_surface_is_exactly_the_documented_writes() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    let surface = e2e::method_surface(&client, &mock, &json!([]))
        .await
        .unwrap();
    insta::assert_json_snapshot!("method_surface", surface);
    for (tool, methods) in surface.as_object().unwrap() {
        let expected = match tool.as_str() {
            "set_state" | "call_service" | "render_template" | "check_config" => json!(["POST"]),
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
        let mut cfg = config(&uri);
        cfg.allowed_hosts = allowed_hosts;
        hamcp::router(&cfg)
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn insecure_flag_is_applied_both_ways() {
    let mock = MockServer::start().await;
    mount(
        &mock,
        "GET",
        "/api/",
        ok(&json!({ "message": "API running." })),
    )
    .await;
    e2e::check_insecure_flag(
        &mock,
        |base_url, insecure| {
            let mut cfg = config(base_url);
            cfg.insecure = insecure;
            hamcp::router(&cfg)
        },
        "health_check",
        json!({}),
    )
    .await
    .unwrap();
}

/// "Dim the living room to 30% for movie night, and check what the evening
/// looks like." Read, act, confirm, then the surrounding context tools.
#[tokio::test]
// One scenario, read top to bottom; splitting it would hide the story.
#[allow(clippy::too_many_lines)]
async fn movie_night() {
    let mock = MockServer::start().await;
    mount(
        &mock,
        "GET",
        "/api/",
        ok(&json!({ "message": "API running." })),
    )
    .await;
    mount(&mock, "GET", "/api/config", ok(&json!({
        "latitude": 52.52, "longitude": 13.405, "elevation": 34,
        "unit_system": { "length": "km", "accumulated_precipitation": "mm", "area": "m²", "mass": "g",
            "pressure": "Pa", "temperature": "°C", "volume": "L", "wind_speed": "m/s" },
        "location_name": "Home", "time_zone": "Europe/Berlin", "components": ["calendar", "history", "light"],
        "config_dir": "/config", "whitelist_external_dirs": ["/media", "/config/www"],
        "allowlist_external_dirs": ["/media", "/config/www"], "allowlist_external_urls": [],
        "version": "2024.9.3", "config_source": "storage", "recovery_mode": false, "state": "RUNNING",
        "external_url": null, "internal_url": "http://homeassistant.local:8123", "currency": "EUR",
        "country": "DE", "language": "en", "safe_mode": false, "debug": false, "radius": 100
    }))).await;
    mount(&mock, "GET", "/api/states", ok(&json!([light("on", Some(255)),
        { "entity_id": "sensor.living_room_temperature", "state": "21.4", "attributes": { "unit_of_measurement": "°C" },
          "last_changed": "2024-09-25T17:55:00+00:00", "last_updated": "2024-09-25T17:55:00+00:00" }]))).await;
    mount(
        &mock,
        "GET",
        "/api/states/light.living_room",
        ok(&light("on", Some(255))),
    )
    .await;
    mount(&mock, "GET", "/api/services", ok(&json!([
        { "domain": "light", "services": { "turn_on": { "name": "Turn on", "fields": { "brightness_pct": { "selector": { "number": { "min": 0, "max": 100 } } } } } } }
    ]))).await;
    mount(
        &mock,
        "POST",
        "/api/services/light/turn_on",
        ok(&json!([light("on", Some(77))])),
    )
    .await;
    mount(&mock, "POST", "/api/states/input_boolean.movie_mode", ok(&json!({
        "entity_id": "input_boolean.movie_mode", "state": "on", "attributes": { "source": "hamcp" },
        "last_changed": "2024-09-25T20:00:00+00:00", "last_updated": "2024-09-25T20:00:00+00:00"
    }))).await;
    mount(
        &mock,
        "POST",
        "/api/template",
        ResponseTemplate::new(200)
            .set_body_raw("Living Room is on at 30%", "text/plain; charset=utf-8"),
    )
    .await;
    mount(
        &mock,
        "GET",
        "/api/calendars",
        ok(&json!([{ "entity_id": "calendar.family", "name": "Family" }])),
    )
    .await;
    mount(&mock, "GET", "/api/calendars/calendar.family", ok(&json!([
        { "summary": "Movie night", "start": { "dateTime": "2024-09-25T20:00:00+02:00" }, "end": { "dateTime": "2024-09-25T23:00:00+02:00" },
          "description": null, "location": "Living room" },
        { "summary": "Recycling", "start": { "date": "2024-09-26" }, "end": { "date": "2024-09-27" } }
    ]))).await;
    mount(&mock, "GET", "/api/history/period/2024-09-25T00%3A00%3A00%2B00%3A00", ok(&json!([[
        { "entity_id": "light.living_room", "state": "off", "last_changed": "2024-09-25T06:00:00+00:00" },
        { "state": "on", "last_changed": "2024-09-25T18:02:11+00:00" }
    ]]))).await;
    mount(&mock, "GET", "/api/history/period", ok(&json!([[]]))).await;
    mount(
        &mock,
        "POST",
        "/api/config/core/check_config",
        ok(&json!({ "result": "valid", "errors": null, "warnings": null })),
    )
    .await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    for (tool, args) in [
        ("health_check", json!({})),
        ("get_config", json!({})),
        ("get_states", json!({})),
        ("get_entity", json!({ "entity_id": "light.living_room" })),
        ("get_services", json!({})),
        // Top-level entity_id wins over the one in service_data, and the rest
        // of service_data is kept.
        (
            "call_service",
            json!({ "domain": "light", "service": "turn_on", "entity_id": "light.living_room",
            "service_data": { "entity_id": "light.kitchen", "brightness_pct": 30, "transition": 2 } }),
        ),
        (
            "set_state",
            json!({ "entity_id": "input_boolean.movie_mode", "state": "on", "attributes": { "source": "hamcp" } }),
        ),
        (
            "render_template",
            json!({ "template": "{{ state_attr('light.living_room', 'friendly_name') }} is {{ states('light.living_room') }} at 30%" }),
        ),
        ("get_calendars", json!({})),
        (
            "get_calendar_events",
            json!({ "entity_id": "calendar.family", "start": "2024-09-25T00:00:00+02:00", "end": "2024-09-27T00:00:00+02:00" }),
        ),
        (
            "get_history",
            json!({ "entity_ids": ["light.living_room", "sensor.living_room_temperature"],
            "start_time": "2024-09-25T00:00:00+00:00", "end_time": "2024-09-25T23:59:59+00:00",
            "minimal_response": true, "no_attributes": true }),
        ),
        (
            "get_history",
            json!({ "entity_ids": ["light.living_room"] }),
        ),
        ("check_config", json!({})),
    ] {
        let res = s.call(tool, args.clone()).await.unwrap();
        assert!(res.is_success(), "{tool} {args}: {:?}", res.message());
    }
    insta::assert_json_snapshot!("movie_night", s.transcript());
}

/// Command failures, and a read of each failing shape. A write that failed
/// must say so; the session must survive every one.
#[tokio::test]
// One table of failure shapes against one live session, on purpose.
#[allow(clippy::too_many_lines)]
async fn failures_are_never_reported_as_success() {
    let mock = MockServer::start().await;
    mount(
        &mock,
        "POST",
        "/api/services/light/turn_on",
        ResponseTemplate::new(500).set_body_raw(
            "500 Internal Server Error\n\nServer got itself in trouble",
            "text/plain",
        ),
    )
    .await;
    mount(
        &mock,
        "POST",
        "/api/services/light/flash",
        ResponseTemplate::new(400)
            .set_body_json(json!({ "message": "Service does not support responses" })),
    )
    .await;
    mount(
        &mock,
        "GET",
        "/api/states/light.ghost",
        ResponseTemplate::new(404).set_body_json(json!({ "message": "Entity not found." })),
    )
    .await;
    mount(
        &mock,
        "GET",
        "/api/config",
        ResponseTemplate::new(401).set_body_raw("401: Unauthorized", "text/plain"),
    )
    .await;
    mount(
        &mock,
        "GET",
        "/api/states",
        ResponseTemplate::new(502).set_body_raw(
            "<html><body><h1>502 Bad Gateway</h1></body></html>",
            "text/html",
        ),
    )
    .await;
    mount(
        &mock,
        "GET",
        "/api/services",
        ResponseTemplate::new(200).set_body_raw(r#"[{"domain":"light","servi"#, "application/json"),
    )
    .await;
    mount(
        &mock,
        "POST",
        "/api/template",
        ResponseTemplate::new(400).set_body_json(
            json!({ "message": "Error rendering template: UndefinedError: 'foo' is undefined" }),
        ),
    )
    .await;
    mount(
        &mock,
        "GET",
        "/api/",
        ok(&json!({ "message": "API running." })),
    )
    .await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    for (tool, args) in [
        (
            "call_service",
            json!({ "domain": "light", "service": "turn_on", "entity_id": "light.living_room" }),
        ),
        (
            "call_service",
            json!({ "domain": "light", "service": "flash" }),
        ),
        ("get_entity", json!({ "entity_id": "light.ghost" })),
        ("get_config", json!({})),
        ("get_states", json!({})),
        ("get_services", json!({})),
        ("render_template", json!({ "template": "{{ foo.bar }}" })),
        // Path escapes, refused before anything is sent.
        ("set_state", json!({ "entity_id": "..", "state": "on" })),
        (
            "call_service",
            json!({ "domain": "..", "service": "states" }),
        ),
        ("get_entity", json!({ "entity_id": "." })),
    ] {
        let res = s.call(tool, args.clone()).await.unwrap();
        assert!(!res.is_success(), "{tool} {args} must fail");
        s.call("health_check", json!({}))
            .await
            .unwrap()
            .text()
            .unwrap();
    }
    insta::assert_json_snapshot!("failures", s.transcript());

    // `/` inside an entity ID stays inside its segment: one request, to the
    // encoded path, never to `/api/states/light/x`.
    let slash = MockServer::start().await;
    mount(
        &slash,
        "GET",
        "/api/states/light%2Fx%3Fy%23z",
        ok(&light("off", None)),
    )
    .await;
    let (_s2, c2) = serve(&config(&slash.uri())).await.unwrap();
    c2.call_tool("get_entity", json!({ "entity_id": "light/x?y#z" }))
        .await
        .unwrap()
        .text()
        .unwrap();
}

/// An error page longer than the 512-byte cap whose cut point lands inside a
/// multi-byte character. Truncating by byte index would panic the request
/// handler, a crash the whole server feels (hard rule §7). It must come back
/// as an ordinary, truncated tool error, and the next call must still work.
#[tokio::test]
async fn long_non_ascii_error_pages_are_truncated_without_panicking() {
    let mock = MockServer::start().await;
    // 511 ASCII bytes, then "ü" (2 bytes) straddling byte 512.
    let page = format!(
        "{}ü and the rest of a very long German error page: Ungültige Anfrage",
        "x".repeat(511)
    );
    mount(
        &mock,
        "GET",
        "/api/states",
        ResponseTemplate::new(500).set_body_raw(page, "text/plain; charset=utf-8"),
    )
    .await;
    mount(
        &mock,
        "GET",
        "/api/",
        ok(&json!({ "message": "API running." })),
    )
    .await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let msg = client
        .call_tool("get_states", json!({}))
        .await
        .unwrap()
        .error_message()
        .unwrap();
    assert!(msg.contains("500") && msg.contains("(truncated)"), "{msg}");
    client
        .call_tool("health_check", json!({}))
        .await
        .unwrap()
        .text()
        .unwrap();
}

/// A template Home Assistant cannot render must come back with HA's own
/// reason, or the LLM has nothing to fix its template with.
#[tokio::test]
async fn template_errors_carry_home_assistants_reason() {
    let mock = MockServer::start().await;
    mount(
        &mock,
        "POST",
        "/api/template",
        ResponseTemplate::new(400).set_body_json(
            json!({ "message": "Error rendering template: UndefinedError: 'foo' is undefined" }),
        ),
    )
    .await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    let msg = client
        .call_tool("render_template", json!({ "template": "{{ foo.bar }}" }))
        .await
        .unwrap()
        .error_message()
        .unwrap();
    assert!(msg.contains("'foo' is undefined"), "{msg}");
}

#[tokio::test]
async fn unreachable_home_assistant_is_a_prompt_tool_error() {
    let dead = format!("http://{}", e2e::free_loopback_addr().unwrap());
    let (_server, client) = serve(&config(&dead)).await.unwrap();

    let started = std::time::Instant::now();
    let res = client
        .call_tool(
            "call_service",
            json!({ "domain": "light", "service": "turn_off" }),
        )
        .await
        .unwrap();
    assert!(
        !res.is_success(),
        "a command to a dead host must not succeed"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(12));
}
