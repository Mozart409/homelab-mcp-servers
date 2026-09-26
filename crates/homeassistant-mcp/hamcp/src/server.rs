//! MCP server: exposes Home Assistant control and query tools.

use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    Implementation, ListResourcesResult, PromptMessage, Role, ServerCapabilities, ServerConfig,
};
use rmcp::{
    ErrorData, ServerHandler, prompt, prompt_handler, prompt_router, schemars, tool, tool_handler,
    tool_router,
};
use serde::{Deserialize, Serialize};

use crate::client::{ClientError, HaClient};
use crate::models::inputs::{
    CallServiceInput, GetCalendarEventsInput, GetEntityInput, GetHistoryInput, RenderTemplateInput,
    SetStateInput,
};

/// Map a [`ClientError`] onto an MCP error.
///
/// A refused path segment is the caller's mistake (an entity ID of `..`), so it
/// is `invalid_params`, which tells the client which argument to fix. Everything
/// else is an upstream or transport failure and stays `internal_error`.
fn client_error(e: ClientError) -> ErrorData {
    match e {
        ClientError::InvalidPathSegment(msg) => ErrorData::invalid_params(msg, None),
        // The full chain: the cause (refused, TLS, DNS) sits below reqwest's
        // generic top-level message, and `Display` alone would drop it.
        other => ErrorData::internal_error(mcp_common::error_chain(&other), None),
    }
}

/// MCP server wrapping a [`HaClient`].
#[derive(Clone, Debug)]
pub struct HaServer {
    client: HaClient,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl HaServer {
    /// Wrap a configured client as an MCP server.
    #[must_use]
    pub fn new(client: HaClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
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
        let result = self.client.check_health().await.map_err(client_error)?;

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
        let config = self.client.get_config().await.map_err(client_error)?;
        Self::to_json(&config)
    }

    #[tool(
        name = "get_states",
        description = "Get all Home Assistant entity states including lights, sensors, switches, etc."
    )]
    async fn get_states_tool(&self) -> Result<String, ErrorData> {
        let states = self.client.get_states().await.map_err(client_error)?;
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
            .map_err(client_error)?;
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
            .map_err(|e| match e {
                ClientError::InvalidPathSegment(msg) => ErrorData::invalid_params(msg, None),
                other => ErrorData::internal_error(
                    format!(
                        "Failed to call {}.{}: {}",
                        input.domain,
                        input.service,
                        mcp_common::error_chain(&other)
                    ),
                    None,
                ),
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
            .map_err(client_error)?;
        Self::to_json(&state)
    }

    #[tool(
        name = "get_services",
        description = "Get all available Home Assistant services grouped by domain"
    )]
    async fn get_services_tool(&self) -> Result<String, ErrorData> {
        let services = self.client.get_services().await.map_err(client_error)?;
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
            .map_err(client_error)
    }

    #[tool(
        name = "get_calendars",
        description = "Get all available calendar entities"
    )]
    async fn get_calendars_tool(&self) -> Result<String, ErrorData> {
        let calendars = self.client.get_calendars().await.map_err(client_error)?;
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
            .map_err(client_error)?;
        Self::to_json(&events)
    }

    #[tool(
        name = "check_config",
        description = "Validate the Home Assistant configuration.yaml file"
    )]
    async fn check_config_tool(&self) -> Result<String, ErrorData> {
        let result = self.client.check_config().await.map_err(client_error)?;
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
            .map_err(client_error)?;
        Self::to_json(&history)
    }
}

// ---- Prompt arguments -------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EntityDiagnosticArgs {
    /// Entity to diagnose, e.g. `sensor.living_room_temperature`.
    entity_id: String,
    /// How many hours of history to review (default: 24).
    ///
    /// A string, not a number: MCP prompt arguments are always strings on the
    /// wire (`{[name]: string}`), so a numeric type here would reject every
    /// spec-compliant client's `"48"`. A value that is not a positive whole
    /// number falls back to the default.
    #[serde(default)]
    hours: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AutomationAuditArgs {
    /// Restrict the survey to one domain, e.g. `automation`, `light`, `sensor`.
    /// Omit to survey the whole installation.
    #[serde(default)]
    domain: Option<String>,
}

// ---- Prompts ----------------------------------------------------------------

/// Repeated Home Assistant workflows, encoded as prompts.
///
/// These live in their own inherent `impl` block so `#[prompt_router]` and
/// `#[tool_router]` each own one block outright; neither macro is then asked to
/// walk attributes it does not recognise.
///
/// Both prompts are **diagnostic and read-only in intent**. This is the one
/// server in the workspace that may mutate its target (`set_state`,
/// `call_service`), and an operator running a diagnostic does not expect their
/// lights to change — so these prompts direct the model to gather and reason,
/// and to hand any corrective action back to the human rather than firing it.
#[prompt_router]
impl HaServer {
    /// Decide whether one entity is healthy, stuck, flapping, or simply gone.
    #[prompt(
        name = "entity_diagnostic",
        description = "Diagnose one Home Assistant entity from its current state and history: healthy, stuck, flapping, or unavailable."
    )]
    async fn entity_diagnostic(
        &self,
        params: Parameters<EntityDiagnosticArgs>,
    ) -> Vec<PromptMessage> {
        let EntityDiagnosticArgs { entity_id, hours } = params.0;
        let hours = hours
            .and_then(|h| h.trim().parse::<u32>().ok())
            .filter(|&h| h > 0)
            .unwrap_or(24);

        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Diagnose the health of the Home Assistant entity `{entity_id}` over the last \
                 {hours} hours.\n\n\
                 Work in this order:\n\
                 1. Call `get_entity` for `{entity_id}`. Report its current state verbatim and its \
                 `last_changed` timestamp. If the entity does not exist, stop and say so — do not \
                 guess at a similarly named one.\n\
                 2. Call `get_history` for the same entity over {hours} hours. The history is the \
                 evidence; the current state alone cannot distinguish the cases below.\n\
                 3. Classify what you see into exactly one of:\n\
                 - **Healthy** — the value moves as the device plausibly should.\n\
                 - **Unavailable / unknown** — state is `unavailable` or `unknown`. This almost \
                 always means the integration or device dropped off, not that the reading is \
                 genuinely absent. Say how long it has been in that state.\n\
                 - **Stuck** — the value has not changed across the whole window although this kind \
                 of device should vary. Critically, a stuck sensor and a genuinely steady one look \
                 identical if you only read the current state, which is why step 2 is not optional.\n\
                 - **Flapping** — changing far more often than the device plausibly could.\n\n\
                 Report the actual state and timestamps you observed, not a summary judgement. If \
                 the history is too short or too sparse to separate 'steady' from 'stuck', say that \
                 plainly instead of picking one.\n\n\
                 This is a read-only diagnosis. Do not call `set_state` or `call_service`. If a fix \
                 is warranted, describe it as a recommendation for the operator to approve."
            ),
        )]
    }

    /// Survey the installation for dead entities and leftovers, read-only.
    #[prompt(
        name = "automation_audit",
        description = "Read-only survey of a Home Assistant installation: unavailable entities, automations that never fire, and orphaned leftovers."
    )]
    async fn automation_audit(
        &self,
        params: Parameters<AutomationAuditArgs>,
    ) -> Vec<PromptMessage> {
        let scope = params.0.domain.map_or_else(
            || "the whole installation".to_string(),
            |d| format!("the `{d}` domain"),
        );

        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Audit {scope} and tell me what has quietly stopped working.\n\n\
                 Work in this order:\n\
                 1. Call `get_config` for the version and basic setup, then `check_config` to \
                 confirm the configuration itself is valid. A broken config explains failures that \
                 would otherwise look like unrelated dead entities, so establish this first.\n\
                 2. Call `get_states` to enumerate entities in scope.\n\
                 3. Pick the entities that look wrong and call `get_history` on a sample. Use \
                 `render_template` when a Jinja expression answers a question more directly than \
                 enumerating states would.\n\n\
                 Surface specifically:\n\
                 - Entities in `unavailable` or `unknown` state, and how long they have been so. \
                 Group them by integration — several dead entities from one integration is one \
                 fault, not several.\n\
                 - Automations that appear never to have triggered.\n\
                 - Duplicate or orphaned entities left behind by integrations that were removed.\n\n\
                 Finish with recommendations the operator can act on. This audit is read-only: do \
                 not call `set_state` or `call_service`, and present every corrective action as \
                 something for a human to approve rather than a step you take."
            ),
        )]
    }
}

// Rust 1.98's `clippy::unused_async_trait_impl` (pedantic, therefore deny here)
// fires four times on this block, and only two of them are ours: `list_resources`
// and `read_resource` genuinely have no `.await`. The other two originate inside
// the `tool_handler` and `prompt_handler` expansions -- rmcp generates async trait
// methods whose bodies are `std::future::ready(..)` -- so there is no source in
// this repo to change. Silencing it per-method would still leave the macro pair
// failing, which is why the allow sits on the whole impl. Revisit when rmcp stops
// generating bodies that never await; the two hand-written methods can drop their
// `async` at that point.
#[allow(clippy::unused_async_trait_impl)]
#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for HaServer {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::default();
        info.instructions = Some(
            "Home Assistant MCP server for controlling your smart home. \
             Provides tools to query entities, call services, set states, \
             inspect calendars, render templates, and check configuration."
                .to_string(),
        );
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_prompts()
            .enable_resources()
            .build();

        let mut server_info = Implementation::default();
        server_info.name = env!("CARGO_PKG_NAME").to_string();
        server_info.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info = server_info;

        info
    }

    /// Advertise this crate's README as the server's operator guide.
    ///
    /// hamcp is the one server here that can mutate its target, and that
    /// exception is the single most important thing a client should know about
    /// it — so the guidance belongs somewhere a client can read without
    /// spending a tool call to find out.
    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let uri = mcp_common::doc_resource_uri(env!("CARGO_PKG_NAME"));
        Ok(ListResourcesResult::with_all_items(vec![
            mcp_common::doc_resource(
                &uri,
                "Home Assistant MCP operator guide",
                "README for homeassistant-mcp: entity/service usage, and the deliberate exception that makes this the one server with mutating tools.",
            ),
        ]))
    }

    /// Serve the operator guide's markdown for the URI advertised above.
    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, ErrorData> {
        let uri = mcp_common::doc_resource_uri(env!("CARGO_PKG_NAME"));
        if request.uri == uri {
            Ok(mcp_common::doc_resource_contents(&uri, include_str!("../../README.md")).into())
        } else {
            Err(ErrorData::resource_not_found(
                format!("unknown resource uri: {}", request.uri),
                None,
            ))
        }
    }
}
