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
