//! MCP server: exposes Home Assistant control and query tools.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use serde::Serialize;

use crate::client::HaClient;
use crate::models::inputs::{
    CallServiceInput, GetCalendarEventsInput, GetEntityInput, GetHistoryInput, RenderTemplateInput,
    SetStateInput,
};

/// MCP server wrapping a [`HaClient`].
#[derive(Clone, Debug)]
pub struct HaServer {
    client: HaClient,
    tool_router: ToolRouter<Self>,
}

impl HaServer {
    /// Wrap a configured client as an MCP server.
    #[must_use]
    pub fn new(client: HaClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
        }
    }

    /// Serialize a value to pretty JSON, mapping errors into MCP `ErrorData`.
    fn to_json<T: Serialize>(value: &T) -> Result<String, ErrorData> {
        serde_json::to_string_pretty(value)
            .map_err(|e| ErrorData::internal_error(format!("JSON serialization error: {e}"), None))
    }
}

#[tool_router]
impl HaServer {
    #[tool(
        name = "health_check",
        description = "Check if the Home Assistant API is running and healthy"
    )]
    async fn health_check_tool(&self) -> Result<String, ErrorData> {
        let result = self
            .client
            .check_health()
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;

        if result.healthy {
            Ok(format!("Home Assistant API is healthy: {}", result.message))
        } else {
            Err(ErrorData::internal_error(
                format!("Home Assistant API is unhealthy: {}", result.message),
                None,
            ))
        }
    }

    #[tool(
        name = "get_config",
        description = "Get Home Assistant configuration including location, unit system, and loaded components"
    )]
    async fn get_config_tool(&self) -> Result<String, ErrorData> {
        let config = self
            .client
            .get_config()
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        Self::to_json(&config)
    }

    #[tool(
        name = "get_states",
        description = "Get all Home Assistant entity states including lights, sensors, switches, etc."
    )]
    async fn get_states_tool(&self) -> Result<String, ErrorData> {
        let states = self
            .client
            .get_states()
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        Self::to_json(&states)
    }

    #[tool(
        name = "get_entity",
        description = "Get the current state of a specific Home Assistant entity by ID"
    )]
    async fn get_entity_tool(
        &self,
        Parameters(input): Parameters<GetEntityInput>,
    ) -> Result<String, ErrorData> {
        let state = self
            .client
            .get_entity(&input.entity_id)
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        Self::to_json(&state)
    }

    #[tool(
        name = "call_service",
        description = "Call a Home Assistant service to control devices. Examples: domain='light', service='turn_on', entity_id='light.living_room'"
    )]
    async fn call_service_tool(
        &self,
        Parameters(input): Parameters<CallServiceInput>,
    ) -> Result<String, ErrorData> {
        let response = self
            .client
            .call_service(
                &input.domain,
                &input.service,
                input.service_data,
                input.entity_id.as_deref(),
                false,
            )
            .await
            .map_err(|e| {
                ErrorData::internal_error(
                    format!("Failed to call {}.{}: {e}", input.domain, input.service),
                    None,
                )
            })?;
        Self::to_json(&response)
    }

    #[tool(
        name = "set_state",
        description = "Set or update a state for a Home Assistant entity. Creates the entity if it doesn't exist."
    )]
    async fn set_state_tool(
        &self,
        Parameters(input): Parameters<SetStateInput>,
    ) -> Result<String, ErrorData> {
        let state_update = crate::models::StateUpdate {
            state: input.state,
            attributes: input.attributes,
        };
        let state = self
            .client
            .set_state(&input.entity_id, &state_update)
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        Self::to_json(&state)
    }

    #[tool(
        name = "get_services",
        description = "Get all available Home Assistant services grouped by domain"
    )]
    async fn get_services_tool(&self) -> Result<String, ErrorData> {
        let services = self
            .client
            .get_services()
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        Self::to_json(&services)
    }

    #[tool(
        name = "render_template",
        description = "Render a Home Assistant template string. Example: 'The temperature is {{ states(\"sensor.temperature\") }}C'"
    )]
    async fn render_template_tool(
        &self,
        Parameters(input): Parameters<RenderTemplateInput>,
    ) -> Result<String, ErrorData> {
        self.client
            .render_template(&input.template)
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))
    }

    #[tool(
        name = "get_calendars",
        description = "Get all available calendar entities"
    )]
    async fn get_calendars_tool(&self) -> Result<String, ErrorData> {
        let calendars = self
            .client
            .get_calendars()
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        Self::to_json(&calendars)
    }

    #[tool(
        name = "get_calendar_events",
        description = "Get events from a specific calendar within a time range"
    )]
    async fn get_calendar_events_tool(
        &self,
        Parameters(input): Parameters<GetCalendarEventsInput>,
    ) -> Result<String, ErrorData> {
        let events = self
            .client
            .get_calendar_events(&input.entity_id, &input.start, &input.end)
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        Self::to_json(&events)
    }

    #[tool(
        name = "check_config",
        description = "Validate the Home Assistant configuration.yaml file"
    )]
    async fn check_config_tool(&self) -> Result<String, ErrorData> {
        let result = self
            .client
            .check_config()
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        Self::to_json(&result)
    }

    #[tool(
        name = "get_history",
        description = "Get historical state data for one or more entities within a time range"
    )]
    async fn get_history_tool(
        &self,
        Parameters(input): Parameters<GetHistoryInput>,
    ) -> Result<String, ErrorData> {
        let history = self
            .client
            .get_history(
                &input.entity_ids,
                input.start_time.as_deref(),
                input.end_time.as_deref(),
                input.minimal_response,
                input.no_attributes,
            )
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        Self::to_json(&history)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for HaServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Home Assistant MCP server for controlling your smart home. \
             Provides tools to query entities, call services, set states, \
             inspect calendars, render templates, and check configuration."
                .to_string(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();

        let mut server_info = Implementation::default();
        server_info.name = env!("CARGO_PKG_NAME").to_string();
        server_info.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info = server_info;

        info
    }
}

/// Wire-level tests for every tool.
///
/// `hamcp` is the one server in this workspace that is allowed to mutate its
/// target (AGENTS.md hard rule §1 exception), so the exact HTTP method, path,
/// query string and body each tool puts on the wire is a correctness property
/// worth pinning, not an implementation detail. Every test here drives a real
/// [`HaClient`] against a `wiremock` server and asserts on the *recorded*
/// request rather than on a matcher, so a failure reports what was actually
/// sent instead of "no mock matched".
///
/// Where the current behaviour looks wrong (see the `quirk_` tests) the test
/// documents reality rather than the intent; changing it is a behaviour change,
/// and these tests exist so that change is a deliberate, visible one.
#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;
    use wiremock::http::Method;
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use super::*;

    /// Token used by every test; the exact header format is asserted below.
    const TOKEN: &str = "test-token-123";

    /// A realistic `GET /api/states/<id>` payload.
    const ENTITY_JSON: &str = r#"{
        "entity_id": "light.living_room",
        "state": "on",
        "attributes": {"friendly_name": "Living Room", "brightness": 128},
        "last_changed": "2026-07-28T11:46:34.000000+00:00",
        "last_updated": "2026-07-28T11:46:34.000000+00:00"
    }"#;

    /// A `GET /api/config` payload trimmed to the fields the model requires.
    const CONFIG_JSON: &str = r#"{
        "components": ["light", "sensor"],
        "config_dir": "/config",
        "elevation": 42.0,
        "latitude": 52.52,
        "longitude": 13.405,
        "location_name": "Home",
        "time_zone": "Europe/Berlin",
        "unit_system": {"length": "km", "mass": "g", "temperature": "°C", "volume": "L"},
        "version": "2026.7.0"
    }"#;

    fn json_status(status: u16, body: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_raw(body.as_bytes().to_vec(), "application/json")
    }

    fn json_ok(body: &str) -> ResponseTemplate {
        json_status(200, body)
    }

    /// Start a mock that answers *every* request with `template`.
    ///
    /// A catch-all keeps the assertions in the test body: the request is
    /// inspected after the fact instead of being encoded into matchers.
    async fn mock_answering(template: ResponseTemplate) -> MockServer {
        let mock = MockServer::start().await;
        Mock::given(any()).respond_with(template).mount(&mock).await;
        mock
    }

    fn ha_server(uri: &str) -> crate::client::Result<HaServer> {
        Ok(HaServer::new(HaClient::new(uri, TOKEN)?))
    }

    /// The single request the mock recorded, if exactly one arrived.
    async fn recorded(mock: &MockServer) -> Option<Request> {
        let mut requests = mock.received_requests().await?.into_iter();
        let first = requests.next()?;
        if requests.next().is_some() {
            return None;
        }
        Some(first)
    }

    fn query_pairs(request: &Request) -> Vec<(String, String)> {
        request
            .url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn auth_header(request: &Request) -> Option<&str> {
        request.headers.get("authorization")?.to_str().ok()
    }

    // ---------------------------------------------------------------------
    // call_service — mutating
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn call_service_posts_to_domain_and_service_path() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        server
            .call_service_tool(Parameters(CallServiceInput {
                domain: "light".to_string(),
                service: "turn_on".to_string(),
                entity_id: Some("light.living_room".to_string()),
                service_data: None,
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(request.method, Method::POST);
        // `seg()` percent-encodes every non-alphanumeric byte, so the service
        // name reaches the wire as `turn%5Fon`, not `turn_on`. Home Assistant
        // decodes the segment, so this is correct-but-surprising.
        assert_eq!(request.url.path(), "/api/services/light/turn%5Fon");
        // `return_response` is hard-coded to false by the tool, so no query.
        assert_eq!(request.url.query(), None);
    }

    #[tokio::test]
    async fn call_service_body_carries_entity_id_when_provided() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        server
            .call_service_tool(Parameters(CallServiceInput {
                domain: "light".to_string(),
                service: "turn_on".to_string(),
                entity_id: Some("light.living_room".to_string()),
                service_data: None,
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(
            request.body_json::<serde_json::Value>().unwrap(),
            json!({"entity_id": "light.living_room"})
        );
    }

    #[tokio::test]
    async fn call_service_body_omits_entity_id_when_none() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        server
            .call_service_tool(Parameters(CallServiceInput {
                domain: "homeassistant".to_string(),
                service: "restart".to_string(),
                entity_id: None,
                service_data: None,
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        // The key is absent entirely — not `"entity_id": null`. Home Assistant
        // rejects a null target, so this matters.
        assert_eq!(request.body_json::<serde_json::Value>().unwrap(), json!({}));
    }

    #[tokio::test]
    async fn call_service_merges_service_data_into_body() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        let mut data = HashMap::new();
        data.insert("brightness".to_string(), json!(255));
        data.insert("transition".to_string(), json!(2.5));

        server
            .call_service_tool(Parameters(CallServiceInput {
                domain: "light".to_string(),
                service: "turn_on".to_string(),
                entity_id: Some("light.kitchen".to_string()),
                service_data: Some(data),
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(
            request.body_json::<serde_json::Value>().unwrap(),
            json!({
                "brightness": 255,
                "transition": 2.5,
                "entity_id": "light.kitchen"
            })
        );
    }

    #[tokio::test]
    async fn call_service_top_level_entity_id_overrides_service_data() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        let mut data = HashMap::new();
        data.insert("entity_id".to_string(), json!("light.from_service_data"));

        server
            .call_service_tool(Parameters(CallServiceInput {
                domain: "light".to_string(),
                service: "turn_on".to_string(),
                entity_id: Some("light.top_level".to_string()),
                service_data: Some(data),
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        // Precedence: the top-level `entity_id` is inserted last and silently
        // replaces any `entity_id` the caller put inside `service_data`.
        assert_eq!(
            request.body_json::<serde_json::Value>().unwrap(),
            json!({"entity_id": "light.top_level"})
        );
    }

    #[tokio::test]
    async fn call_service_keeps_service_data_entity_id_when_top_level_absent() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        let mut data = HashMap::new();
        data.insert(
            "entity_id".to_string(),
            json!(["light.a", "light.b"]), // HA accepts a list target
        );

        server
            .call_service_tool(Parameters(CallServiceInput {
                domain: "light".to_string(),
                service: "turn_off".to_string(),
                entity_id: None,
                service_data: Some(data),
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(
            request.body_json::<serde_json::Value>().unwrap(),
            json!({"entity_id": ["light.a", "light.b"]})
        );
    }

    #[tokio::test]
    async fn call_service_sends_bearer_authorization_header() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        server
            .call_service_tool(Parameters(CallServiceInput {
                domain: "light".to_string(),
                service: "turn_on".to_string(),
                entity_id: None,
                service_data: None,
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(auth_header(&request), Some("Bearer test-token-123"));
    }

    #[tokio::test]
    async fn call_service_bad_request_maps_to_error() {
        let mock = mock_answering(json_status(400, r#"{"message":"nope"}"#)).await;
        let server = ha_server(&mock.uri()).unwrap();

        let result = server
            .call_service_tool(Parameters(CallServiceInput {
                domain: "light".to_string(),
                service: "turn_on".to_string(),
                entity_id: None,
                service_data: None,
            }))
            .await;

        let error = result.unwrap_err();
        assert!(
            error.message.contains("Failed to call light.turn_on"),
            "unexpected message: {}",
            error.message
        );
    }

    // ---------------------------------------------------------------------
    // set_state — mutating
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn set_state_posts_to_percent_encoded_entity_path() {
        let mock = mock_answering(json_ok(ENTITY_JSON)).await;
        let server = ha_server(&mock.uri()).unwrap();

        server
            .set_state_tool(Parameters(SetStateInput {
                entity_id: "light.living_room".to_string(),
                state: "on".to_string(),
                attributes: None,
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(request.method, Method::POST);
        // `seg()` really is applied here: `.` -> `%2E` and `_` -> `%5F`.
        assert_eq!(request.url.path(), "/api/states/light%2Eliving%5Froom");
        assert_eq!(request.url.query(), None);
        assert_eq!(auth_header(&request), Some("Bearer test-token-123"));
    }

    #[tokio::test]
    async fn set_state_entity_id_cannot_escape_its_path_segment() {
        let mock = mock_answering(json_ok(ENTITY_JSON)).await;
        let server = ha_server(&mock.uri()).unwrap();

        let _ = server
            .set_state_tool(Parameters(SetStateInput {
                entity_id: "../config/core/check_config".to_string(),
                state: "on".to_string(),
                attributes: None,
            }))
            .await;

        let request = recorded(&mock).await.unwrap();
        // Path traversal is neutralised by the encoding: this stays one segment.
        assert_eq!(
            request.url.path(),
            "/api/states/%2E%2E%2Fconfig%2Fcore%2Fcheck%5Fconfig"
        );
    }

    #[tokio::test]
    async fn set_state_body_carries_state_and_attributes() {
        let mock = mock_answering(json_ok(ENTITY_JSON)).await;
        let server = ha_server(&mock.uri()).unwrap();

        let mut attributes = HashMap::new();
        attributes.insert("unit_of_measurement".to_string(), json!("°C"));
        attributes.insert("nested".to_string(), json!({"a": [1, 2]}));

        server
            .set_state_tool(Parameters(SetStateInput {
                entity_id: "sensor.custom".to_string(),
                state: "22.5".to_string(),
                attributes: Some(attributes),
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(
            request.body_json::<serde_json::Value>().unwrap(),
            json!({
                "state": "22.5",
                "attributes": {"unit_of_measurement": "°C", "nested": {"a": [1, 2]}}
            })
        );
    }

    #[tokio::test]
    async fn quirk_set_state_sends_null_attributes_when_omitted() {
        let mock = mock_answering(json_ok(ENTITY_JSON)).await;
        let server = ha_server(&mock.uri()).unwrap();

        server
            .set_state_tool(Parameters(SetStateInput {
                entity_id: "sensor.custom".to_string(),
                state: "22.5".to_string(),
                attributes: None,
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        // `StateUpdate::attributes` has `#[serde(default)]` but no
        // `skip_serializing_if`, so an omitted `attributes` is transmitted as an
        // explicit JSON null rather than being left out. Pinned as current
        // behaviour: adding `skip_serializing_if` would change the wire format.
        assert_eq!(
            request.body_json::<serde_json::Value>().unwrap(),
            json!({"state": "22.5", "attributes": null})
        );
    }

    // ---------------------------------------------------------------------
    // Read tools — paths, query strings, auth
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn health_check_gets_api_root() {
        let mock = mock_answering(json_ok(r#"{"message":"API running."}"#)).await;
        let server = ha_server(&mock.uri()).unwrap();

        let output = server.health_check_tool().await.unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.url.path(), "/api/");
        assert_eq!(auth_header(&request), Some("Bearer test-token-123"));
        assert_eq!(output, "Home Assistant API is healthy: API running.");
    }

    #[tokio::test]
    async fn get_config_gets_api_config() {
        let mock = mock_answering(json_ok(CONFIG_JSON)).await;
        let server = ha_server(&mock.uri()).unwrap();

        server.get_config_tool().await.unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.url.path(), "/api/config");
    }

    #[tokio::test]
    async fn get_states_gets_api_states() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        server.get_states_tool().await.unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.url.path(), "/api/states");
        assert_eq!(request.url.query(), None);
        assert_eq!(auth_header(&request), Some("Bearer test-token-123"));
    }

    #[tokio::test]
    async fn get_entity_gets_percent_encoded_entity_path() {
        let mock = mock_answering(json_ok(ENTITY_JSON)).await;
        let server = ha_server(&mock.uri()).unwrap();

        server
            .get_entity_tool(Parameters(GetEntityInput {
                entity_id: "light.living_room".to_string(),
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.url.path(), "/api/states/light%2Eliving%5Froom");
    }

    #[tokio::test]
    async fn get_services_gets_api_services() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        server.get_services_tool().await.unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.url.path(), "/api/services");
    }

    #[tokio::test]
    async fn render_template_posts_template_body_and_returns_raw_text() {
        let mock = mock_answering(ResponseTemplate::new(200).set_body_string("22.5C")).await;
        let server = ha_server(&mock.uri()).unwrap();

        let output = server
            .render_template_tool(Parameters(RenderTemplateInput {
                template: "{{ states('sensor.t') }}C".to_string(),
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        // `render_template` is a POST in the Home Assistant REST API even
        // though it mutates nothing; the tool follows the API.
        assert_eq!(request.method, Method::POST);
        assert_eq!(request.url.path(), "/api/template");
        assert_eq!(
            request.body_json::<serde_json::Value>().unwrap(),
            json!({"template": "{{ states('sensor.t') }}C"})
        );
        // The rendered body is returned verbatim, not JSON-wrapped.
        assert_eq!(output, "22.5C");
    }

    #[tokio::test]
    async fn render_template_error_status_maps_to_error() {
        let mock = mock_answering(json_status(400, "bad template")).await;
        let server = ha_server(&mock.uri()).unwrap();

        let result = server
            .render_template_tool(Parameters(RenderTemplateInput {
                template: "{{".to_string(),
            }))
            .await;

        assert!(result.unwrap_err().message.contains("Template rendering"));
    }

    #[tokio::test]
    async fn get_calendars_gets_api_calendars() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        server.get_calendars_tool().await.unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.url.path(), "/api/calendars");
    }

    #[tokio::test]
    async fn get_calendar_events_sends_start_and_end_query_params() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        server
            .get_calendar_events_tool(Parameters(GetCalendarEventsInput {
                entity_id: "calendar.personal".to_string(),
                start: "2026-01-01T00:00:00".to_string(),
                end: "2026-12-31T23:59:59".to_string(),
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.url.path(), "/api/calendars/calendar%2Epersonal");
        assert_eq!(
            query_pairs(&request),
            vec![
                ("start".to_string(), "2026-01-01T00:00:00".to_string()),
                ("end".to_string(), "2026-12-31T23:59:59".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn get_history_without_start_time_uses_bare_period_path() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        server
            .get_history_tool(Parameters(GetHistoryInput {
                entity_ids: vec![
                    "sensor.temperature".to_string(),
                    "sensor.humidity".to_string(),
                ],
                start_time: None,
                end_time: None,
                minimal_response: false,
                no_attributes: false,
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.url.path(), "/api/history/period");
        // Entity IDs are comma-joined into a single `filter_entity_id` param.
        assert_eq!(
            query_pairs(&request),
            vec![(
                "filter_entity_id".to_string(),
                "sensor.temperature,sensor.humidity".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn get_history_with_start_time_encodes_it_into_the_path() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        server
            .get_history_tool(Parameters(GetHistoryInput {
                entity_ids: vec!["sensor.temperature".to_string()],
                start_time: Some("2026-07-28T11:46:34+00:00".to_string()),
                end_time: Some("2026-07-29T11:46:34+00:00".to_string()),
                minimal_response: false,
                no_attributes: false,
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        assert_eq!(
            request.url.path(),
            "/api/history/period/2026%2D07%2D28T11%3A46%3A34%2B00%3A00"
        );
        assert_eq!(
            query_pairs(&request),
            vec![
                (
                    "filter_entity_id".to_string(),
                    "sensor.temperature".to_string()
                ),
                (
                    "end_time".to_string(),
                    "2026-07-29T11:46:34+00:00".to_string()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn get_history_flags_are_sent_as_valueless_query_params() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        server
            .get_history_tool(Parameters(GetHistoryInput {
                entity_ids: vec!["sensor.temperature".to_string()],
                start_time: None,
                end_time: None,
                minimal_response: true,
                no_attributes: true,
            }))
            .await
            .unwrap();

        let request = recorded(&mock).await.unwrap();
        // Home Assistant treats these as presence flags; they go out as
        // `minimal_response=` (empty value), never `=true`.
        assert_eq!(
            request.url.query(),
            Some("filter_entity_id=sensor.temperature&minimal_response=&no_attributes=")
        );
    }

    #[tokio::test]
    async fn check_config_posts_to_core_check_config() {
        let mock = mock_answering(json_ok(r#"{"result":"valid","errors":null}"#)).await;
        let server = ha_server(&mock.uri()).unwrap();

        server.check_config_tool().await.unwrap();

        let request = recorded(&mock).await.unwrap();
        // POST by Home Assistant API design; it validates, it does not write.
        assert_eq!(request.method, Method::POST);
        assert_eq!(request.url.path(), "/api/config/core/check_config");
    }

    // ---------------------------------------------------------------------
    // Upstream failure mapping — must be `Err(ErrorData)`, never a panic
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn unauthorized_read_maps_to_error() {
        let mock = mock_answering(json_status(401, r#"{"message":"Unauthorized"}"#)).await;
        let server = ha_server(&mock.uri()).unwrap();

        assert!(server.get_states_tool().await.is_err());
    }

    #[tokio::test]
    async fn not_found_entity_maps_to_error() {
        let mock = mock_answering(json_status(404, r#"{"message":"Entity not found."}"#)).await;
        let server = ha_server(&mock.uri()).unwrap();

        let result = server
            .get_entity_tool(Parameters(GetEntityInput {
                entity_id: "light.missing".to_string(),
            }))
            .await;

        assert!(
            result.unwrap_err().message.contains("Entity not found"),
            "404 must surface as an entity-not-found error"
        );
    }

    #[tokio::test]
    async fn not_found_config_maps_to_error() {
        let mock = mock_answering(json_status(404, "{}")).await;
        let server = ha_server(&mock.uri()).unwrap();

        assert!(server.get_config_tool().await.is_err());
    }

    #[tokio::test]
    async fn server_error_maps_to_error_when_body_is_not_the_expected_shape() {
        let mock = mock_answering(json_status(500, "<html>500</html>")).await;
        let server = ha_server(&mock.uri()).unwrap();

        assert!(server.get_config_tool().await.is_err());
        assert!(
            ha_server(&mock.uri())
                .unwrap()
                .get_states_tool()
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn health_check_reports_non_200_as_error() {
        let mock = mock_answering(json_status(500, "{}")).await;
        let server = ha_server(&mock.uri()).unwrap();

        let result = server.health_check_tool().await;
        assert!(
            result.unwrap_err().message.contains("unhealthy"),
            "a non-200 /api/ must map to an unhealthy error"
        );
    }

    #[tokio::test]
    async fn read_tools_fail_on_non_success_status_even_when_the_body_parses() {
        // Regression: the client used to deserialize the body regardless of
        // status, so a 500 whose body happened to be valid JSON was reported as
        // a successful empty result.
        let mock = mock_answering(json_status(500, "[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        let err = server.get_states_tool().await.unwrap_err();
        assert!(
            err.message.contains("500"),
            "error should name the status, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn read_tools_surface_status_and_body_for_an_unparseable_error_page() {
        // The `get_calendars` failure mode observed against a live instance: the
        // calendar integration is not loaded, Home Assistant answers 404, and
        // feeding that to serde produced an opaque "error decoding response
        // body" that named neither the status nor the cause.
        let mock = mock_answering(json_status(404, r#"{"message":"Not found"}"#)).await;
        let server = ha_server(&mock.uri()).unwrap();

        let err = server.get_calendars_tool().await.unwrap_err();
        assert!(
            err.message.contains("404"),
            "error should name the status, got: {}",
            err.message
        );
        assert!(
            err.message.contains("Not found"),
            "error should include the upstream body, got: {}",
            err.message
        );
        assert!(
            !err.message.contains("error decoding response body"),
            "the status must be reported instead of a decode failure, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn call_service_reports_500_as_an_error_not_a_silent_no_op() {
        // Regression, and the most consequential of the set: only 400 was
        // special-cased, so every other error status fell through to a parser
        // whose `unwrap_or_default()` yields an empty `changed_states`. A failed
        // *mutating* call looked like a successful no-op -- on the one server in
        // this workspace permitted to change device state.
        let mock = mock_answering(json_status(500, r#"{"message":"boom"}"#)).await;
        let server = ha_server(&mock.uri()).unwrap();

        let err = server
            .call_service_tool(Parameters(CallServiceInput {
                domain: "light".to_string(),
                service: "turn_on".to_string(),
                entity_id: Some("light.living_room".to_string()),
                service_data: None,
            }))
            .await
            .unwrap_err();

        assert!(
            err.message.contains("500"),
            "error should name the status, got: {}",
            err.message
        );
        assert!(
            !err.message.contains("changed_states"),
            "a failed write must not be reported as a state-change result, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn call_service_still_reports_400_with_its_specific_hint() {
        // The pre-existing 400 branch carries a more useful message than the
        // generic status error, so it must keep taking precedence.
        let mock = mock_answering(json_status(400, r#"{"message":"bad"}"#)).await;
        let server = ha_server(&mock.uri()).unwrap();

        let err = server
            .call_service_tool(Parameters(CallServiceInput {
                domain: "light".to_string(),
                service: "turn_on".to_string(),
                entity_id: None,
                service_data: None,
            }))
            .await
            .unwrap_err();

        assert!(
            err.message.contains("light.turn_on"),
            "400 should keep its specific hint, got: {}",
            err.message
        );
    }

    // ---------------------------------------------------------------------
    // Method audit — the AGENTS.md §1 exception, made explicit
    // ---------------------------------------------------------------------

    /// Responds plausibly to every endpoint the tool sweep touches.
    struct SweepResponder;

    impl wiremock::Respond for SweepResponder {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body = match request.url.path() {
                "/api/" => r#"{"message":"API running."}"#,
                "/api/config" => CONFIG_JSON,
                "/api/config/core/check_config" => r#"{"result":"valid","errors":null}"#,
                "/api/template" => "rendered",
                path if path.starts_with("/api/states/") => ENTITY_JSON,
                _ => "[]",
            };
            json_ok(body)
        }
    }

    /// Every tool, in the order the sweep below invokes them.
    const TOOL_SWEEP_ORDER: [&str; 12] = [
        "health_check",
        "get_config",
        "get_states",
        "get_entity",
        "get_services",
        "get_calendars",
        "get_calendar_events",
        "get_history",
        "render_template",
        "check_config",
        "call_service",
        "set_state",
    ];

    /// Tools that are allowed to leave the read-only path.
    ///
    /// `call_service` and `set_state` are the AGENTS.md hard-rule §1 exception:
    /// they genuinely mutate Home Assistant. `render_template` and
    /// `check_config` are POSTs only because the Home Assistant REST API
    /// models them that way — they change nothing. If this assertion ever
    /// fails, a tool started writing to the target; that needs a human.
    const EXPECTED_NON_GET_TOOLS: [&str; 4] = [
        "render_template",
        "check_config",
        "call_service",
        "set_state",
    ];

    #[tokio::test]
    async fn method_audit_only_expected_tools_issue_non_get_requests() {
        let mock = MockServer::start().await;
        Mock::given(any())
            .respond_with(SweepResponder)
            .mount(&mock)
            .await;
        let server = ha_server(&mock.uri()).unwrap();

        // Sequential awaits, so the recorded order matches TOOL_SWEEP_ORDER.
        server.health_check_tool().await.unwrap();
        server.get_config_tool().await.unwrap();
        server.get_states_tool().await.unwrap();
        server
            .get_entity_tool(Parameters(GetEntityInput {
                entity_id: "light.living_room".to_string(),
            }))
            .await
            .unwrap();
        server.get_services_tool().await.unwrap();
        server.get_calendars_tool().await.unwrap();
        server
            .get_calendar_events_tool(Parameters(GetCalendarEventsInput {
                entity_id: "calendar.personal".to_string(),
                start: "2026-01-01T00:00:00".to_string(),
                end: "2026-01-02T00:00:00".to_string(),
            }))
            .await
            .unwrap();
        server
            .get_history_tool(Parameters(GetHistoryInput {
                entity_ids: vec!["sensor.temperature".to_string()],
                start_time: None,
                end_time: None,
                minimal_response: false,
                no_attributes: false,
            }))
            .await
            .unwrap();
        server
            .render_template_tool(Parameters(RenderTemplateInput {
                template: "{{ 1 }}".to_string(),
            }))
            .await
            .unwrap();
        server.check_config_tool().await.unwrap();
        server
            .call_service_tool(Parameters(CallServiceInput {
                domain: "light".to_string(),
                service: "turn_on".to_string(),
                entity_id: Some("light.living_room".to_string()),
                service_data: None,
            }))
            .await
            .unwrap();
        server
            .set_state_tool(Parameters(SetStateInput {
                entity_id: "sensor.custom".to_string(),
                state: "1".to_string(),
                attributes: None,
            }))
            .await
            .unwrap();

        let requests = mock.received_requests().await.unwrap();
        assert_eq!(
            requests.len(),
            TOOL_SWEEP_ORDER.len(),
            "each tool must issue exactly one upstream request"
        );

        let non_get: Vec<&str> = TOOL_SWEEP_ORDER
            .iter()
            .zip(requests.iter())
            .filter(|(_, request)| request.method != Method::GET)
            .map(|(name, _)| *name)
            .collect();

        assert_eq!(non_get, EXPECTED_NON_GET_TOOLS.to_vec());

        // Nothing may ever use a destructive verb.
        for request in &requests {
            assert!(
                request.method == Method::GET || request.method == Method::POST,
                "unexpected method {} on {}",
                request.method,
                request.url.path()
            );
        }
    }

    #[tokio::test]
    async fn every_tool_sends_the_bearer_token() {
        let mock = MockServer::start().await;
        Mock::given(any())
            .respond_with(SweepResponder)
            .mount(&mock)
            .await;
        let server = ha_server(&mock.uri()).unwrap();

        server.get_states_tool().await.unwrap();
        server
            .set_state_tool(Parameters(SetStateInput {
                entity_id: "sensor.custom".to_string(),
                state: "1".to_string(),
                attributes: None,
            }))
            .await
            .unwrap();

        let requests = mock.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        for request in &requests {
            assert_eq!(
                request
                    .headers
                    .get("authorization")
                    .map(|v| v.to_str().ok()),
                Some(Some("Bearer test-token-123"))
            );
        }
    }

    #[tokio::test]
    async fn server_info_advertises_tools_and_crate_metadata() {
        let mock = mock_answering(json_ok("[]")).await;
        let server = ha_server(&mock.uri()).unwrap();

        let info = server.get_info();
        assert!(info.capabilities.tools.is_some());
        assert_eq!(info.server_info.name, "hamcp");
        assert!(
            info.instructions
                .unwrap_or_default()
                .contains("Home Assistant MCP server")
        );
    }
}
