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

/// Deserialization tests for the tool input types.
///
/// An MCP client sends these as JSON, so what matters is which fields are
/// genuinely optional and what an omitted field becomes. Every test below
/// deserializes a payload an MCP client could realistically send.
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn get_entity_input_requires_entity_id() {
        let input: GetEntityInput =
            serde_json::from_str(r#"{"entity_id": "light.living_room"}"#).unwrap();
        assert_eq!(input.entity_id, "light.living_room");

        let missing: Result<GetEntityInput, _> = serde_json::from_str("{}");
        assert!(missing.is_err());
    }

    #[test]
    fn call_service_input_defaults_optional_fields_to_none() {
        let input: CallServiceInput =
            serde_json::from_str(r#"{"domain": "light", "service": "turn_on"}"#).unwrap();

        assert_eq!(input.domain, "light");
        assert_eq!(input.service, "turn_on");
        assert!(input.entity_id.is_none());
        assert!(input.service_data.is_none());
    }

    #[test]
    fn call_service_input_deserializes_full_payload() {
        let input: CallServiceInput = serde_json::from_str(
            r#"{
                "domain": "light",
                "service": "turn_on",
                "entity_id": "light.living_room",
                "service_data": {"brightness": 255, "rgb_color": [255, 0, 0]}
            }"#,
        )
        .unwrap();

        assert_eq!(input.entity_id.as_deref(), Some("light.living_room"));
        let data = input.service_data.unwrap();
        assert_eq!(data.get("brightness"), Some(&json!(255)));
        assert_eq!(data.get("rgb_color"), Some(&json!([255, 0, 0])));
    }

    #[test]
    fn call_service_input_requires_domain_and_service() {
        let no_service: Result<CallServiceInput, _> =
            serde_json::from_str(r#"{"domain": "light"}"#);
        assert!(no_service.is_err());

        let no_domain: Result<CallServiceInput, _> =
            serde_json::from_str(r#"{"service": "turn_on"}"#);
        assert!(no_domain.is_err());
    }

    #[test]
    fn call_service_input_ignores_unknown_fields() {
        // No `deny_unknown_fields`: a client sending an extra key is tolerated
        // rather than rejected.
        let input: CallServiceInput = serde_json::from_str(
            r#"{"domain": "light", "service": "turn_on", "target": {"area_id": "kitchen"}}"#,
        )
        .unwrap();
        assert!(input.service_data.is_none());
    }

    #[test]
    fn set_state_input_defaults_attributes_to_none() {
        let input: SetStateInput =
            serde_json::from_str(r#"{"entity_id": "sensor.custom", "state": "22.5"}"#).unwrap();

        assert_eq!(input.entity_id, "sensor.custom");
        assert_eq!(input.state, "22.5");
        assert!(input.attributes.is_none());
    }

    #[test]
    fn set_state_input_deserializes_attributes() {
        let input: SetStateInput = serde_json::from_str(
            r#"{
                "entity_id": "sensor.custom",
                "state": "22.5",
                "attributes": {"unit_of_measurement": "°C", "nested": {"a": 1}}
            }"#,
        )
        .unwrap();

        let attributes = input.attributes.unwrap();
        assert_eq!(attributes.get("nested"), Some(&json!({"a": 1})));
    }

    #[test]
    fn set_state_input_requires_entity_id_and_state() {
        let no_state: Result<SetStateInput, _> =
            serde_json::from_str(r#"{"entity_id": "sensor.custom"}"#);
        assert!(no_state.is_err());

        let no_entity: Result<SetStateInput, _> = serde_json::from_str(r#"{"state": "22.5"}"#);
        assert!(no_entity.is_err());
    }

    #[test]
    fn render_template_input_requires_template() {
        let input: RenderTemplateInput =
            serde_json::from_str(r#"{"template": "{{ states('sensor.t') }}"}"#).unwrap();
        assert_eq!(input.template, "{{ states('sensor.t') }}");

        let missing: Result<RenderTemplateInput, _> = serde_json::from_str("{}");
        assert!(missing.is_err());
    }

    #[test]
    fn get_calendar_events_input_requires_all_three_fields() {
        let input: GetCalendarEventsInput = serde_json::from_str(
            r#"{
                "entity_id": "calendar.personal",
                "start": "2026-01-01T00:00:00",
                "end": "2026-12-31T23:59:59"
            }"#,
        )
        .unwrap();
        assert_eq!(input.entity_id, "calendar.personal");
        assert_eq!(input.start, "2026-01-01T00:00:00");
        assert_eq!(input.end, "2026-12-31T23:59:59");

        let missing_end: Result<GetCalendarEventsInput, _> = serde_json::from_str(
            r#"{"entity_id": "calendar.personal", "start": "2026-01-01T00:00:00"}"#,
        );
        assert!(missing_end.is_err());
    }

    #[test]
    fn get_history_input_defaults_flags_to_false() {
        let input: GetHistoryInput =
            serde_json::from_str(r#"{"entity_ids": ["sensor.temperature"]}"#).unwrap();

        assert_eq!(input.entity_ids, vec!["sensor.temperature".to_string()]);
        assert!(input.start_time.is_none());
        assert!(input.end_time.is_none());
        assert!(!input.minimal_response);
        assert!(!input.no_attributes);
    }

    #[test]
    fn get_history_input_requires_entity_ids() {
        // `entity_ids` has no `#[serde(default)]`, so it cannot be omitted —
        // the client must always name the entities to fetch.
        let missing: Result<GetHistoryInput, _> =
            serde_json::from_str(r#"{"start_time": "2026-07-28T00:00:00+00:00"}"#);
        assert!(missing.is_err());
    }

    #[test]
    fn get_history_input_deserializes_full_payload() {
        let input: GetHistoryInput = serde_json::from_str(
            r#"{
                "entity_ids": ["sensor.temperature", "sensor.humidity"],
                "start_time": "2026-07-28T00:00:00+00:00",
                "end_time": "2026-07-29T00:00:00+00:00",
                "minimal_response": true,
                "no_attributes": true
            }"#,
        )
        .unwrap();

        assert_eq!(input.entity_ids.len(), 2);
        assert_eq!(
            input.start_time.as_deref(),
            Some("2026-07-28T00:00:00+00:00")
        );
        assert!(input.minimal_response);
        assert!(input.no_attributes);
    }

    #[test]
    fn get_history_input_accepts_an_empty_entity_id_list() {
        // Nothing rejects it here; the client joins it into an empty
        // `filter_entity_id` query param.
        let input: GetHistoryInput = serde_json::from_str(r#"{"entity_ids": []}"#).unwrap();
        assert!(input.entity_ids.is_empty());
    }
}
