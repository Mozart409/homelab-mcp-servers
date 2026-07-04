//! REST API client for Home Assistant.
//!
//! Provides a type-safe HTTP client for interacting with the Home Assistant
//! REST API, handling authentication, connection pooling, and error handling.
//!
//! Every public async method returns `Result<T, ClientError>`. The error
//! variants on [`ClientError`] are descriptive; repeating an `# Errors` section
//! on each thin REST wrapper would be pure boilerplate.
#![allow(clippy::missing_errors_doc)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client, StatusCode, header};
use url::Url;

use crate::models::{
    ApiStatus, Calendar, CalendarEvent, Config, ConfigCheckResult, EntityState, Event,
    HealthCheckResult, HistoryEntry, ServiceDomain, ServiceResponse, StateUpdate, TemplateRequest,
};

/// Default request timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// HTTP client for interacting with the Home Assistant REST API.
///
/// Cheap to clone due to internal `Arc` usage.
#[derive(Debug, Clone)]
pub struct HaClient {
    base_url: Url,
    client: Arc<Client>,
}

/// Errors that can occur in the REST client.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// HTTP request failed.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// Entity not found.
    #[error("Entity not found: {0}")]
    EntityNotFound(String),

    /// Config endpoint not available.
    #[error("Config endpoint not found - ensure Home Assistant is properly configured")]
    ConfigNotFound,

    /// Invalid authentication token.
    #[error("Invalid token format for Authorization header")]
    InvalidToken,

    /// Failed to create HTTP client.
    #[error("Failed to create HTTP client: {0}")]
    ClientCreationFailed(String),

    /// Invalid URL.
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    /// Service call failed.
    #[error("Service call failed: {0}")]
    ServiceError(String),

    /// Template rendering failed.
    #[error("Template rendering failed: {0}")]
    TemplateError(String),

    /// Error log endpoint not available.
    #[error("Error log endpoint not available - check that logger integration is enabled")]
    ErrorLogNotAvailable,
}

/// Result type for REST client operations.
pub type Result<T> = std::result::Result<T, ClientError>;

impl HaClient {
    /// Creates a new Home Assistant API client.
    ///
    /// # Errors
    ///
    /// Returns an error if the URL is invalid or the HTTP client cannot be created.
    pub fn new(base_url: &str, token: &str) -> Result<Self> {
        let base_url = Url::parse(base_url)
            .map_err(|e| ClientError::InvalidUrl(format!("{base_url}: {e}")))?;

        let mut headers = header::HeaderMap::new();
        let auth_value = format!("Bearer {token}");
        let auth_header =
            header::HeaderValue::from_str(&auth_value).map_err(|_| ClientError::InvalidToken)?;
        headers.insert(header::AUTHORIZATION, auth_header);

        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|e| ClientError::ClientCreationFailed(e.to_string()))?;

        Ok(Self {
            base_url,
            client: Arc::new(client),
        })
    }

    /// Returns a reference to the base URL.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Builds a URL for an API endpoint.
    fn api_url(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(path)
            .map_err(|e| ClientError::InvalidUrl(format!("Failed to build URL for {path}: {e}")))
    }

    /// Checks if the Home Assistant API is healthy.
    pub async fn check_health(&self) -> Result<HealthCheckResult> {
        let url = self.api_url("/api/")?;

        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;

        let status = response.status();

        if status == StatusCode::OK {
            let api_status: ApiStatus = response.json().await.map_err(ClientError::Http)?;
            Ok(HealthCheckResult {
                healthy: true,
                message: api_status.message,
            })
        } else {
            Ok(HealthCheckResult {
                healthy: false,
                message: format!("API returned status: {status}"),
            })
        }
    }

    /// Gets the current configuration of Home Assistant.
    pub async fn get_config(&self) -> Result<Config> {
        let url = self.api_url("/api/config")?;
        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;

        if response.status() == StatusCode::NOT_FOUND {
            return Err(ClientError::ConfigNotFound);
        }
        response.json().await.map_err(ClientError::Http)
    }

    /// Gets all entity states.
    pub async fn get_states(&self) -> Result<Vec<EntityState>> {
        let url = self.api_url("/api/states")?;
        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;
        response.json().await.map_err(ClientError::Http)
    }

    /// Gets a specific entity's state.
    pub async fn get_entity(&self, entity_id: &str) -> Result<EntityState> {
        let url = self.api_url(&format!("/api/states/{entity_id}"))?;
        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;

        if response.status() == StatusCode::NOT_FOUND {
            return Err(ClientError::EntityNotFound(entity_id.to_string()));
        }
        response.json().await.map_err(ClientError::Http)
    }

    /// Sets a state for an entity (creates or updates).
    pub async fn set_state(
        &self,
        entity_id: &str,
        state_update: &StateUpdate,
    ) -> Result<EntityState> {
        let url = self.api_url(&format!("/api/states/{entity_id}"))?;
        let response = self
            .client
            .post(url.as_str())
            .json(state_update)
            .send()
            .await
            .map_err(ClientError::Http)?;
        response.json().await.map_err(ClientError::Http)
    }

    /// Deletes an entity state.
    pub async fn delete_state(&self, entity_id: &str) -> Result<()> {
        let url = self.api_url(&format!("/api/states/{entity_id}"))?;
        let response = self
            .client
            .delete(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;

        if response.status() == StatusCode::NOT_FOUND {
            return Err(ClientError::EntityNotFound(entity_id.to_string()));
        }
        Ok(())
    }

    /// Gets all available services.
    pub async fn get_services(&self) -> Result<Vec<ServiceDomain>> {
        let url = self.api_url("/api/services")?;
        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;
        response.json().await.map_err(ClientError::Http)
    }

    /// Calls a service.
    pub async fn call_service(
        &self,
        domain: &str,
        service: &str,
        service_data: Option<HashMap<String, serde_json::Value>>,
        entity_id: Option<&str>,
        return_response: bool,
    ) -> Result<ServiceResponse> {
        let mut url = self.api_url(&format!("/api/services/{domain}/{service}"))?;
        if return_response {
            url.set_query(Some("return_response"));
        }

        let mut payload = service_data.unwrap_or_default();
        if let Some(eid) = entity_id {
            payload.insert("entity_id".to_string(), serde_json::json!(eid));
        }

        let response = self
            .client
            .post(url.as_str())
            .json(&payload)
            .send()
            .await
            .map_err(ClientError::Http)?;

        let status = response.status();
        if status == StatusCode::BAD_REQUEST {
            return Err(ClientError::ServiceError(format!(
                "Bad request for {domain}.{service} - service may not support response data or invalid parameters"
            )));
        }

        let response_data: serde_json::Value = response.json().await.map_err(ClientError::Http)?;
        let changed_states = response_data
            .get("changed_states")
            .and_then(|states| serde_json::from_value(states.clone()).ok())
            .or_else(|| serde_json::from_value::<Vec<EntityState>>(response_data.clone()).ok())
            .unwrap_or_default();
        let service_response = response_data
            .get("service_response")
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        Ok(ServiceResponse {
            changed_states,
            service_response,
        })
    }

    /// Gets all available events.
    pub async fn get_events(&self) -> Result<Vec<Event>> {
        let url = self.api_url("/api/events")?;
        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;
        response.json().await.map_err(ClientError::Http)
    }

    /// Fires an event.
    pub async fn fire_event(
        &self,
        event_type: &str,
        event_data: Option<HashMap<String, serde_json::Value>>,
    ) -> Result<HashMap<String, String>> {
        let url = self.api_url(&format!("/api/events/{event_type}"))?;
        let response = self
            .client
            .post(url.as_str())
            .json(&event_data.unwrap_or_default())
            .send()
            .await
            .map_err(ClientError::Http)?;
        response.json().await.map_err(ClientError::Http)
    }

    /// Renders a template.
    pub async fn render_template(&self, template: &str) -> Result<String> {
        let url = self.api_url("/api/template")?;
        let request = TemplateRequest {
            template: template.to_string(),
        };
        let response = self
            .client
            .post(url.as_str())
            .json(&request)
            .send()
            .await
            .map_err(ClientError::Http)?;

        if response.status().is_success() {
            response.text().await.map_err(ClientError::Http)
        } else {
            Err(ClientError::TemplateError(format!(
                "Template rendering failed with status: {}",
                response.status()
            )))
        }
    }

    /// Gets all calendars.
    pub async fn get_calendars(&self) -> Result<Vec<Calendar>> {
        let url = self.api_url("/api/calendars")?;
        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;
        response.json().await.map_err(ClientError::Http)
    }

    /// Gets calendar events for a specific calendar.
    pub async fn get_calendar_events(
        &self,
        entity_id: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<CalendarEvent>> {
        let mut url = self.api_url(&format!("/api/calendars/{entity_id}"))?;
        url.query_pairs_mut()
            .append_pair("start", start)
            .append_pair("end", end);

        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;
        response.json().await.map_err(ClientError::Http)
    }

    /// Triggers a configuration check.
    pub async fn check_config(&self) -> Result<ConfigCheckResult> {
        let url = self.api_url("/api/config/core/check_config")?;
        let response = self
            .client
            .post(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;
        response.json().await.map_err(ClientError::Http)
    }

    /// Gets history for entities.
    pub async fn get_history(
        &self,
        entity_ids: &[String],
        start_time: Option<&str>,
        end_time: Option<&str>,
        minimal_response: bool,
        no_attributes: bool,
    ) -> Result<Vec<Vec<HistoryEntry>>> {
        let path = match start_time {
            Some(start) => format!("/api/history/period/{start}"),
            None => "/api/history/period".to_string(),
        };
        let mut url = self.api_url(&path)?;

        {
            let mut query = url.query_pairs_mut();
            query.append_pair("filter_entity_id", &entity_ids.join(","));
            if let Some(end) = end_time {
                query.append_pair("end_time", end);
            }
            if minimal_response {
                query.append_pair("minimal_response", "");
            }
            if no_attributes {
                query.append_pair("no_attributes", "");
            }
        }

        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;
        response.json().await.map_err(ClientError::Http)
    }

    /// Gets the error log.
    pub async fn get_error_log(&self) -> Result<String> {
        let url = self.api_url("/api/error_log")?;
        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;

        if response.status() == StatusCode::NOT_FOUND {
            return Err(ClientError::ErrorLogNotAvailable);
        }
        response.text().await.map_err(ClientError::Http)
    }

    /// Gets camera image data.
    pub async fn get_camera_image(&self, entity_id: &str) -> Result<Vec<u8>> {
        let url = self.api_url(&format!("/api/camera_proxy/{entity_id}"))?;
        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(ClientError::Http)?;

        if response.status() == StatusCode::NOT_FOUND {
            return Err(ClientError::EntityNotFound(entity_id.to_string()));
        }
        response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(ClientError::Http)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation_valid() {
        let client = HaClient::new("http://localhost:8123", "test_token");
        assert!(client.is_ok());
        let client = client.unwrap();
        assert_eq!(client.base_url().as_str(), "http://localhost:8123/");
    }

    #[test]
    fn test_client_creation_invalid_url() {
        let result = HaClient::new("not a valid url", "test_token");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ClientError::InvalidUrl(_)));
    }

    #[test]
    fn test_api_url_building() {
        let client = HaClient::new("http://localhost:8123", "test_token").unwrap();
        let url = client.api_url("/api/states").unwrap();
        assert_eq!(url.as_str(), "http://localhost:8123/api/states");
    }

    #[test]
    fn test_url_with_trailing_slash() {
        let client = HaClient::new("http://localhost:8123/", "test_token").unwrap();
        let url = client.api_url("/api/states").unwrap();
        assert_eq!(url.as_str(), "http://localhost:8123/api/states");
    }
}
