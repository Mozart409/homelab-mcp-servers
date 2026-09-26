//! Thin async HTTP client for the Woodpecker CI HTTP API.

use color_eyre::eyre::{Result, WrapErr, bail};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;

use crate::config::Config;

/// Percent-encode one URL path segment, refusing values that would address a
/// different endpoint (empty, `.`, `..`). See [`mcp_common::path_segment`] for
/// the failure modes; this only maps the refusal to an MCP `invalid_params`
/// error so the caller is told which argument was wrong.
pub(crate) fn seg(s: &str) -> Result<String, rmcp::ErrorData> {
    mcp_common::path_segment(s).map_err(|e| rmcp::ErrorData::invalid_params(e.to_string(), None))
}

/// Client for a single Woodpecker CI instance.
///
/// Cheap to clone (wraps an `Arc` internally via [`reqwest::Client`]).
#[derive(Clone)]
pub struct WpClient {
    http: reqwest::Client,
    base_url: String,
}

impl WpClient {
    /// Construct a client, baking a bearer-token `Authorization` header
    /// into the underlying [`reqwest::Client`].
    ///
    /// # Errors
    ///
    /// Returns an error if the token contains invalid header characters or the
    /// HTTP client cannot be built.
    pub fn new(config: &Config) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let mut auth_val = HeaderValue::from_str(&format!("Bearer {}", config.token))
            .wrap_err("WP_TOKEN contains invalid header characters")?;
        auth_val.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth_val);

        // rustls has no default provider in this workspace; see
        // `mcp_common::install_crypto_provider`.
        mcp_common::install_crypto_provider();

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .danger_accept_invalid_certs(config.insecure)
            .user_agent(concat!("wpmcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .wrap_err("failed to build HTTP client")?;

        Ok(Self {
            http,
            base_url: config.base_url.clone(),
        })
    }

    /// `GET {base_url}{path}` with optional query params, returning the parsed
    /// JSON response as-is. Woodpecker has no success/error envelope — the HTTP
    /// status is the signal.
    ///
    /// An empty body on a success status (e.g. `/api/healthz`) returns
    /// `Value::Null` rather than a parse error.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, the HTTP status is not success,
    /// the body is malformed JSON on a success status, or the API returns an
    /// error message in the response body.
    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.http.get(&url);
        if !query.is_empty() {
            req = req.query(query);
        }

        let resp = req
            .send()
            .await
            .wrap_err_with(|| format!("request to {url} failed"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .wrap_err_with(|| format!("failed to read response body from {url}"))?;

        if !status.is_success() {
            // Try to extract a message from the JSON body; fall back to trimmed raw body.
            let message = if let Ok(json) = serde_json::from_str::<Value>(&body) {
                json.get("message")
                    .or_else(|| json.get("error"))
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| body.trim())
                    .to_string()
            } else {
                body.trim().to_string()
            };
            // Woodpecker answers some errors (404 in particular) with an empty
            // body, and appending an empty message left a dangling `": "` that
            // reads like the error was truncated. Only add the separator when
            // there is something after it.
            if message.is_empty() {
                bail!("Woodpecker API {url} returned {status}");
            }
            bail!("Woodpecker API {url} returned {status}: {message}");
        }

        // Empty body on success is legal (e.g. /api/healthz); treat as null.
        if body.trim().is_empty() {
            return Ok(Value::Null);
        }

        // Parse the body as JSON. A malformed body on a 2xx is an error
        // mentioning the URL.
        serde_json::from_str(&body).wrap_err_with(|| format!("invalid JSON from {url}"))
    }
}
