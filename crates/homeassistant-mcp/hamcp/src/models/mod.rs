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

/// Serde shape tests.
///
/// These types are the contract between the Home Assistant REST API and every
/// tool response, so the failure mode that matters is *shape drift*: Home
/// Assistant changes (or always did) send a field in a form the model does not
/// accept, and a tool starts returning a deserialization error at runtime. The
/// tests therefore feed realistic HA payloads in as JSON literals rather than
/// building structs by hand.
///
/// Tests prefixed `drift_` document a payload Home Assistant really sends that
/// the current model does *not* handle. They assert today's behaviour so the
/// gap is visible and any fix is a deliberate, reviewed change.
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Verbatim-shaped `GET /api/states/<id>` response, including the `context`
    /// object that Home Assistant always sends and the model does not model.
    const ENTITY_STATE_JSON: &str = r#"{
        "entity_id": "light.living_room",
        "state": "on",
        "attributes": {
            "friendly_name": "Living Room",
            "brightness": 128,
            "supported_color_modes": ["brightness"],
            "rgb_color": null,
            "device_info": {"manufacturer": "Acme", "sw_version": "1.2.3"}
        },
        "last_changed": "2026-07-28T11:46:34.123456+00:00",
        "last_updated": "2026-07-28T11:46:35.123456+00:00",
        "context": {"id": "01J0", "parent_id": null, "user_id": null}
    }"#;

    #[test]
    fn entity_state_deserializes_realistic_payload() {
        let state: EntityState = serde_json::from_str(ENTITY_STATE_JSON).unwrap();

        assert_eq!(state.entity_id, "light.living_room");
        assert_eq!(state.state, "on");
        assert_eq!(
            state.last_changed.as_deref(),
            Some("2026-07-28T11:46:34.123456+00:00")
        );
        assert_eq!(
            state.last_updated.as_deref(),
            Some("2026-07-28T11:46:35.123456+00:00")
        );
    }

    #[test]
    fn entity_state_ignores_unknown_top_level_fields() {
        // There is no `deny_unknown_fields` and no `flatten`, so HA's `context`
        // object is silently dropped rather than breaking the parse or leaking
        // into `attributes`.
        let state: EntityState = serde_json::from_str(ENTITY_STATE_JSON).unwrap();
        assert!(!state.attributes.contains_key("context"));
        assert_eq!(state.attributes.len(), 5);
    }

    #[test]
    fn entity_state_timestamps_default_to_none_when_absent() {
        let state: EntityState =
            serde_json::from_str(r#"{"entity_id": "sensor.x", "state": "1", "attributes": {}}"#)
                .unwrap();

        assert!(state.last_changed.is_none());
        assert!(state.last_updated.is_none());
    }

    #[test]
    fn entity_state_attributes_preserve_nested_objects_and_nulls() {
        let state: EntityState = serde_json::from_str(ENTITY_STATE_JSON).unwrap();

        assert_eq!(state.attributes.get("brightness"), Some(&json!(128)));
        assert_eq!(
            state.attributes.get("rgb_color"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(
            state.attributes.get("supported_color_modes"),
            Some(&json!(["brightness"]))
        );
        assert_eq!(
            state.attributes.get("device_info"),
            Some(&json!({"manufacturer": "Acme", "sw_version": "1.2.3"}))
        );
    }

    #[test]
    fn entity_state_requires_attributes() {
        // `attributes` has no `#[serde(default)]`, so a payload without it is a
        // hard error. Home Assistant always sends the key on /api/states, but
        // this is the tightest requirement on the type.
        let result: Result<EntityState, _> =
            serde_json::from_str(r#"{"entity_id": "sensor.x", "state": "1"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn entity_state_round_trips_through_serde() {
        let state: EntityState = serde_json::from_str(ENTITY_STATE_JSON).unwrap();
        let value = serde_json::to_value(&state).unwrap();
        let back: EntityState = serde_json::from_value(value.clone()).unwrap();

        assert_eq!(serde_json::to_value(&back).unwrap(), value);
        // The re-serialized form drops `context` — the tool output is the
        // model's shape, not Home Assistant's.
        assert!(value.get("context").is_none());
    }

    #[test]
    fn api_status_deserializes_and_ignores_extra_fields() {
        let status: ApiStatus =
            serde_json::from_str(r#"{"message": "API running.", "extra": 1}"#).unwrap();
        assert_eq!(status.message, "API running.");
    }

    #[test]
    fn health_check_result_round_trips() {
        let result: HealthCheckResult =
            serde_json::from_str(r#"{"healthy": true, "message": "API running."}"#).unwrap();
        assert!(result.healthy);
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({"healthy": true, "message": "API running."})
        );
    }

    #[test]
    fn config_deserializes_realistic_payload() {
        // Trimmed from a real `GET /api/config`, keeping fields the model does
        // not declare (`state`, `whitelist_external_dirs`, `currency`, and the
        // extra unit-system keys) to prove they are ignored.
        let config: Config = serde_json::from_str(
            r#"{
                "components": ["light", "sensor", "recorder"],
                "config_dir": "/config",
                "elevation": 42,
                "latitude": 52.52,
                "longitude": 13.405,
                "location_name": "Home",
                "time_zone": "Europe/Berlin",
                "unit_system": {
                    "length": "km",
                    "accumulated_precipitation": "mm",
                    "mass": "g",
                    "pressure": "Pa",
                    "temperature": "°C",
                    "volume": "L",
                    "wind_speed": "m/s"
                },
                "version": "2026.7.0",
                "currency": "EUR",
                "state": "RUNNING",
                "whitelist_external_dirs": ["/media"]
            }"#,
        )
        .unwrap();

        assert_eq!(config.location_name, "Home");
        assert_eq!(config.version, "2026.7.0");
        assert_eq!(config.components.len(), 3);
        assert_eq!(config.unit_system.temperature, "\u{b0}C");
        // An integer JSON number still deserializes into the f64 fields.
        assert!((config.elevation - 42.0).abs() < f64::EPSILON);
        assert!((config.latitude - 52.52).abs() < f64::EPSILON);
    }

    #[test]
    fn service_domain_deserializes_real_home_assistant_payload() {
        // Regression: `GET /api/services` returns `services` as an *object*
        // keyed by service name, not an array of names. When this was typed as
        // `Vec<String>` the real payload failed to deserialize and
        // `get_services` errored against every live instance -- confirmed
        // against a running Home Assistant before the fix.
        let domains: Vec<ServiceDomain> = serde_json::from_str(
            r#"[{
                "domain": "light",
                "services": {
                    "turn_on": {"name": "Turn on", "description": "", "fields": {}},
                    "turn_off": {"name": "Turn off", "description": "", "fields": {}}
                }
            }]"#,
        )
        .unwrap();

        let first = domains.first().unwrap();
        assert_eq!(first.domain, "light");

        let mut names: Vec<&str> = first.services.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["turn_off", "turn_on"]);

        // The per-service descriptor is kept whole rather than flattened, so
        // callers can still read `fields`/`target` without a model change.
        let turn_on = first.services.get("turn_on").unwrap();
        assert_eq!(turn_on.get("name"), Some(&serde_json::json!("Turn on")));
        assert!(turn_on.get("fields").is_some());
    }

    #[test]
    fn service_domain_accepts_a_domain_with_no_services() {
        let domains: Vec<ServiceDomain> =
            serde_json::from_str(r#"[{"domain": "persistent_notification", "services": {}}]"#)
                .unwrap();
        assert!(domains.first().unwrap().services.is_empty());
    }

    #[test]
    fn service_response_defaults_service_response_to_none() {
        let response: ServiceResponse = serde_json::from_str(r#"{"changed_states": []}"#).unwrap();
        assert!(response.changed_states.is_empty());
        assert!(response.service_response.is_none());
    }

    #[test]
    fn service_call_defaults_optional_fields_to_none() {
        let call: ServiceCall =
            serde_json::from_str(r#"{"domain": "light", "service": "turn_on"}"#).unwrap();
        assert!(call.entity_id.is_none());
        assert!(call.service_data.is_none());
    }

    #[test]
    fn state_update_serializes_absent_attributes_as_explicit_null() {
        // `attributes` is `#[serde(default)]` but has no `skip_serializing_if`,
        // so `set_state` puts `"attributes": null` on the wire. See the
        // matching `quirk_` test in `server.rs`.
        let update = StateUpdate {
            state: "on".to_string(),
            attributes: None,
        };
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            json!({"state": "on", "attributes": null})
        );
    }

    #[test]
    fn state_update_serializes_attributes_when_present() {
        let mut attributes = HashMap::new();
        attributes.insert("friendly_name".to_string(), json!("Custom"));
        let update = StateUpdate {
            state: "22.5".to_string(),
            attributes: Some(attributes),
        };
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            json!({"state": "22.5", "attributes": {"friendly_name": "Custom"}})
        );
    }

    #[test]
    fn template_request_and_response_shapes() {
        let request = TemplateRequest {
            template: "{{ 1 + 1 }}".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"template": "{{ 1 + 1 }}"})
        );

        let response: TemplateResponse = serde_json::from_str(r#"{"rendered": "2"}"#).unwrap();
        assert_eq!(response.rendered, "2");
    }

    #[test]
    fn calendar_deserializes_list_payload() {
        let calendars: Vec<Calendar> =
            serde_json::from_str(r#"[{"entity_id": "calendar.personal", "name": "Personal"}]"#)
                .unwrap();
        assert_eq!(calendars.first().unwrap().name, "Personal");
    }

    #[test]
    fn calendar_event_deserializes_all_day_event() {
        let events: Vec<CalendarEvent> = serde_json::from_str(
            r#"[{
                "summary": "Holiday",
                "start": {"date": "2026-05-01"},
                "end": {"date": "2026-05-02"},
                "description": null,
                "location": null
            }]"#,
        )
        .unwrap();

        let event = events.first().unwrap();
        assert_eq!(event.summary, "Holiday");
        assert_eq!(event.start.date.as_deref(), Some("2026-05-01"));
        assert!(event.start.date_time.is_none());
        assert!(event.description.is_none());
    }

    #[test]
    fn calendar_event_deserializes_home_assistant_camel_case_date_time() {
        // Regression: Home Assistant's calendar API emits `dateTime` (camelCase,
        // inherited from the CalDAV/Google shape). Without the `#[serde(rename)]`
        // the parse still *succeeded* with both fields `None`, so every timed
        // event came back with no time at all -- silent data loss rather than an
        // error, which is why it went unnoticed.
        let event: CalendarEvent = serde_json::from_str(
            r#"{
                "summary": "Standup",
                "start": {"dateTime": "2026-05-01T09:00:00+02:00"},
                "end": {"dateTime": "2026-05-01T09:15:00+02:00"},
                "location": "Office"
            }"#,
        )
        .unwrap();

        assert_eq!(
            event.start.date_time.as_deref(),
            Some("2026-05-01T09:00:00+02:00")
        );
        assert_eq!(
            event.end.date_time.as_deref(),
            Some("2026-05-01T09:15:00+02:00")
        );
        assert!(event.start.date.is_none());
        assert_eq!(event.location.as_deref(), Some("Office"));
    }

    #[test]
    fn calendar_time_round_trips_as_camel_case() {
        // `rename` applies to serialization too, so a re-serialized event stays
        // consumable by anything expecting Home Assistant's own shape.
        let time = CalendarTime {
            date: None,
            date_time: Some("2026-05-01T09:00:00+02:00".to_string()),
        };
        let json = serde_json::to_value(&time).unwrap();
        assert!(
            json.get("dateTime").is_some(),
            "expected camelCase key, got {json}"
        );
        assert!(json.get("date_time").is_none());
    }

    #[test]
    fn config_check_result_deserializes_valid_and_invalid() {
        let valid: ConfigCheckResult =
            serde_json::from_str(r#"{"result": "valid", "errors": null}"#).unwrap();
        assert_eq!(valid.result, "valid");
        assert!(valid.errors.is_none());

        let invalid: ConfigCheckResult =
            serde_json::from_str(r#"{"result": "invalid", "errors": "bad indentation"}"#).unwrap();
        assert_eq!(invalid.errors.as_deref(), Some("bad indentation"));

        // `errors` is `#[serde(default)]`, so an omitted key is fine too.
        let terse: ConfigCheckResult = serde_json::from_str(r#"{"result": "valid"}"#).unwrap();
        assert!(terse.errors.is_none());
    }

    #[test]
    fn event_deserializes_listener_counts() {
        let events: Vec<Event> =
            serde_json::from_str(r#"[{"event": "state_changed", "listener_count": 12}]"#).unwrap();
        assert_eq!(events.first().unwrap().listener_count, 12);
    }

    #[test]
    fn history_deserializes_nested_arrays() {
        // `/api/history/period` returns one array per requested entity.
        let history: Vec<Vec<HistoryEntry>> = serde_json::from_str(
            r#"[[
                {
                    "entity_id": "sensor.temperature",
                    "state": "21.5",
                    "attributes": {"unit_of_measurement": "°C"},
                    "last_changed": "2026-07-28T11:46:34+00:00",
                    "last_updated": "2026-07-28T11:46:34+00:00"
                }
            ]]"#,
        )
        .unwrap();

        let entry = history.first().and_then(|series| series.first()).unwrap();
        assert_eq!(entry.entity_id.as_deref(), Some("sensor.temperature"));
        assert_eq!(entry.state, "21.5");
        assert!(entry.attributes.is_some());
    }

    #[test]
    fn history_entry_attributes_default_to_none() {
        // What `no_attributes=` produces: the key is simply absent.
        let entry: HistoryEntry = serde_json::from_str(
            r#"{
                "entity_id": "sensor.temperature",
                "state": "21.5",
                "last_changed": "2026-07-28T11:46:34+00:00"
            }"#,
        )
        .unwrap();

        assert!(entry.attributes.is_none());
        assert!(entry.last_updated.is_none());
    }

    #[test]
    fn history_entry_deserializes_minimal_response_payload() {
        // Regression: with `minimal_response=`, Home Assistant sends the first
        // entry of a series in full and reduces the rest to
        // `{state, last_changed}`. While `entity_id` was a required `String`
        // those minimal entries failed to deserialize, so the whole
        // `get_history` call errored whenever the flag was set -- confirmed
        // against a live instance, where the same query succeeded with
        // `minimal_response: false` and failed with `true`.
        let entry: HistoryEntry = serde_json::from_str(
            r#"{"state": "21.6", "last_changed": "2026-07-28T11:47:00+00:00"}"#,
        )
        .unwrap();

        assert!(entry.entity_id.is_none());
        assert_eq!(entry.state, "21.6");
        assert!(entry.attributes.is_none());
        assert!(entry.last_updated.is_none());
    }

    #[test]
    fn history_series_mixes_full_and_minimal_entries() {
        // The exact shape `minimal_response=` returns: entry one is complete and
        // carries the entity_id for the series, the rest are reduced. Both must
        // land in the same `Vec<HistoryEntry>`.
        let history: Vec<Vec<HistoryEntry>> = serde_json::from_str(
            r#"[[
                {
                    "entity_id": "sensor.temperature",
                    "state": "21.5",
                    "attributes": {"unit_of_measurement": "°C"},
                    "last_changed": "2026-07-28T11:46:34+00:00",
                    "last_updated": "2026-07-28T11:46:34+00:00"
                },
                {"state": "21.6", "last_changed": "2026-07-28T11:47:00+00:00"},
                {"state": "21.7", "last_changed": "2026-07-28T11:48:00+00:00"}
            ]]"#,
        )
        .unwrap();

        let series = history.first().unwrap();
        let [full, minimal, last] = series.as_slice() else {
            panic!("expected exactly 3 entries, got {}", series.len())
        };

        assert_eq!(full.entity_id.as_deref(), Some("sensor.temperature"));
        assert!(full.attributes.is_some());
        // Later entries identify their entity by series position, not by field.
        assert!(minimal.entity_id.is_none());
        assert_eq!(minimal.state, "21.6");
        assert_eq!(last.state, "21.7");
    }
}
