//! Data models for Home Assistant API responses.
//!
//! This module contains all the data structures used for serializing
//! and deserializing Home Assistant API responses.

pub mod inputs;

use std::collections::HashMap;

use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// Result of a health check operation on the Home Assistant API.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HealthCheckResult {
    /// Whether the API is healthy.
    pub healthy: bool,
    /// Status message from the API.
    pub message: String,
}

/// Response from the Home Assistant API status endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiStatus {
    /// Status message from the API.
    pub message: String,
}

/// Represents the state of a Home Assistant entity.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EntityState {
    /// The entity ID (e.g., `light.living_room`).
    pub entity_id: String,
    /// The current state value (e.g., "on", "off", "22.5").
    pub state: String,
    /// Additional attributes of the entity.
    pub attributes: HashMap<String, serde_json::Value>,
    /// Last time the state changed.
    #[serde(default)]
    pub last_changed: Option<String>,
    /// Last time the state was updated.
    #[serde(default)]
    pub last_updated: Option<String>,
}

/// Home Assistant configuration information.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Config {
    /// List of loaded components.
    pub components: Vec<String>,
    /// Configuration directory path.
    pub config_dir: String,
    /// Elevation in meters.
    pub elevation: f64,
    /// Latitude coordinate.
    pub latitude: f64,
    /// Longitude coordinate.
    pub longitude: f64,
    /// Location name.
    pub location_name: String,
    /// Time zone.
    pub time_zone: String,
    /// Unit system configuration.
    pub unit_system: UnitSystem,
    /// Home Assistant version.
    pub version: String,
}

/// Unit system configuration.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UnitSystem {
    /// Length unit (e.g., "km", "mi").
    pub length: String,
    /// Mass unit (e.g., "g", "lb").
    pub mass: String,
    /// Temperature unit (e.g., "°C", "°F").
    pub temperature: String,
    /// Volume unit (e.g., "L", "gal").
    pub volume: String,
}

/// Service domain with available services.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServiceDomain {
    /// The domain name (e.g., "light", "switch").
    pub domain: String,
    /// Services in this domain, keyed by service name.
    ///
    /// `GET /api/services` returns this as an *object* — `{"turn_on": {...},
    /// "turn_off": {...}}` — not a list of names. Each value carries the
    /// service's `name`, `description`, `fields`, and `target`, whose shapes
    /// vary per integration, so they stay as raw JSON rather than being pinned
    /// to a struct that HA would eventually outgrow.
    pub services: HashMap<String, serde_json::Value>,
}

/// Service call request data.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServiceCall {
    /// The domain of the service (e.g., "light").
    pub domain: String,
    /// The service name (e.g., `turn_on`).
    pub service: String,
    /// Service data/parameters.
    #[serde(default)]
    pub service_data: Option<HashMap<String, serde_json::Value>>,
    /// Target entity ID.
    #[serde(default)]
    pub entity_id: Option<String>,
}

/// Service call response containing changed states.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServiceResponse {
    /// States that changed during the service call.
    pub changed_states: Vec<EntityState>,
    /// Optional service response data.
    #[serde(default)]
    pub service_response: Option<HashMap<String, serde_json::Value>>,
}

/// State update request for setting entity state.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StateUpdate {
    /// The new state value.
    pub state: String,
    /// Optional attributes to set.
    #[serde(default)]
    pub attributes: Option<HashMap<String, serde_json::Value>>,
}

/// Template rendering request.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemplateRequest {
    /// The template string to render.
    pub template: String,
}

/// Template rendering response.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemplateResponse {
    /// The rendered template output.
    pub rendered: String,
}

/// Calendar entity information.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Calendar {
    /// Calendar entity ID.
    pub entity_id: String,
    /// Calendar name.
    pub name: String,
}

/// Calendar event.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CalendarEvent {
    /// Event summary/title.
    pub summary: String,
    /// Start time information.
    pub start: CalendarTime,
    /// End time information.
    pub end: CalendarTime,
    /// Optional description.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional location.
    #[serde(default)]
    pub location: Option<String>,
}

/// Calendar event time information.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CalendarTime {
    /// Date for all-day events (YYYY-MM-DD format).
    #[serde(default)]
    pub date: Option<String>,
    /// `DateTime` for timed events.
    ///
    /// Home Assistant emits this key as camelCase `dateTime`. Without the
    /// rename the field silently stayed `None` for every timed event and the
    /// parse still *succeeded* — losing the timestamp with no error at all,
    /// which is why this went unnoticed.
    #[serde(default, rename = "dateTime")]
    pub date_time: Option<String>,
}

/// Configuration check result.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConfigCheckResult {
    /// Check result ("valid" or "invalid").
    pub result: String,
    /// Error messages if invalid.
    #[serde(default)]
    pub errors: Option<String>,
}

/// Event information.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Event {
    /// Event name.
    pub event: String,
    /// Number of listeners.
    pub listener_count: u32,
}

/// History entry for entity state changes.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryEntry {
    /// Entity ID.
    ///
    /// Optional because `minimal_response=true` omits it: Home Assistant sends
    /// the first entry of each series in full, then reduces every subsequent
    /// entry to `{state, last_changed}`. With this field required, the whole
    /// `get_history` call failed to deserialize whenever that flag was set.
    /// The entity is identified by the position of its series in the response,
    /// matching the order of the requested `entity_ids`.
    #[serde(default)]
    pub entity_id: Option<String>,
    /// State value.
    pub state: String,
    /// Attributes.
    #[serde(default)]
    pub attributes: Option<HashMap<String, serde_json::Value>>,
    /// Last changed timestamp.
    pub last_changed: String,
    /// Last updated timestamp.
    #[serde(default)]
    pub last_updated: Option<String>,
}
