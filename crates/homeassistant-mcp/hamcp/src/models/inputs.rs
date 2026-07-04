//! Input types for MCP tool handlers.
//!
//! These types define the expected input schema for each MCP tool,
//! allowing for automatic schema generation and validation.

use std::collections::HashMap;

use rmcp::schemars;
use serde::Deserialize;

/// Input for the `get_entity` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetEntityInput {
    /// The entity ID (e.g., `light.living_room`).
    pub entity_id: String,
}

/// Input for the `call_service` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CallServiceInput {
    /// The service domain (e.g., `light`, `switch`, `climate`).
    pub domain: String,
    /// The service name (e.g., `turn_on`, `turn_off`, `set_temperature`).
    pub service: String,
    /// Optional entity ID to target (e.g., `light.living_room`).
    #[serde(default)]
    pub entity_id: Option<String>,
    /// Optional service data parameters (e.g., `{"brightness": 255}`).
    #[serde(default)]
    pub service_data: Option<HashMap<String, serde_json::Value>>,
}

/// Input for the `set_state` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetStateInput {
    /// The entity ID (e.g., `sensor.custom_sensor`).
    pub entity_id: String,
    /// The state value to set.
    pub state: String,
    /// Optional attributes to set.
    #[serde(default)]
    pub attributes: Option<HashMap<String, serde_json::Value>>,
}

/// Input for the `render_template` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RenderTemplateInput {
    /// The template string to render.
    ///
    /// Example: `"The temperature is {{ states('sensor.temperature') }}C"`
    pub template: String,
}

/// Input for the `get_calendar_events` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetCalendarEventsInput {
    /// The calendar entity ID (e.g., `calendar.personal`).
    pub entity_id: String,
    /// Start time in ISO 8601 format (e.g., `2024-01-01T00:00:00`).
    pub start: String,
    /// End time in ISO 8601 format (e.g., `2024-12-31T23:59:59`).
    pub end: String,
}

/// Input for the `get_history` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetHistoryInput {
    /// Entity IDs to fetch history for (e.g., `["sensor.temperature"]`).
    pub entity_ids: Vec<String>,
    /// Optional start time in ISO 8601 format.
    #[serde(default)]
    pub start_time: Option<String>,
    /// Optional end time in ISO 8601 format.
    #[serde(default)]
    pub end_time: Option<String>,
    /// Return only changed states (faster, default: false).
    #[serde(default)]
    pub minimal_response: bool,
    /// Skip returning attributes (faster, default: false).
    #[serde(default)]
    pub no_attributes: bool,
}
