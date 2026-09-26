//! Thin async HTTP client for the Alertmanager v2 HTTP API.

use color_eyre::eyre::{Result, WrapErr, bail};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;

use crate::config::Config;

/// Cap on how much of an error body is quoted back, so a stray HTML page from a
/// reverse proxy cannot flood the MCP transcript.
const MAX_ERROR_BODY: usize = 512;

/// Percent-encode one URL path segment, refusing values that would address a
/// different endpoint (empty, `.`, `..`). See [`mcp_common::path_segment`] for
/// the failure modes; this only maps the refusal to an MCP `invalid_params`
/// error so the caller is told which argument was wrong.
pub(crate) fn seg(s: &str) -> Result<String, rmcp::ErrorData> {
    mcp_common::path_segment(s).map_err(|e| rmcp::ErrorData::invalid_params(e.to_string(), None))
}

/// Client for a single Alertmanager instance.
///
/// Cheap to clone (wraps an `Arc` internally via [`reqwest::Client`]).
#[derive(Clone)]
pub struct AlertmanagerClient {
    http: reqwest::Client,
    base_url: String,
}

impl AlertmanagerClient {
    /// Construct a client, baking an optional bearer-token `Authorization` header
    /// into the underlying [`reqwest::Client`].
    ///
    /// # Errors
    ///
    /// Returns an error if the token contains invalid header characters or the
    /// HTTP client cannot be built.
    pub fn new(config: &Config) -> Result<Self> {
        let mut headers = HeaderMap::new();
        if let Some(token) = &config.token {
            let mut auth_val = HeaderValue::from_str(&format!("Bearer {token}"))
                .wrap_err("ALERTMANAGER_TOKEN contains invalid header characters")?;
            auth_val.set_sensitive(true);
            headers.insert(AUTHORIZATION, auth_val);
        }

        // rustls has no default provider in this workspace; see
        // `mcp_common::install_crypto_provider`.
        mcp_common::install_crypto_provider();

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .danger_accept_invalid_certs(config.insecure)
            .user_agent(concat!("alertmanagermcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .wrap_err("failed to build HTTP client")?;

        Ok(Self {
            http,
            base_url: config.base_url.clone(),
        })
    }

    /// `GET {base_url}{path}` with optional query params, returning the parsed
    /// JSON body. Query keys may repeat (e.g. `filter`), which is how
    /// Alertmanager expects multi-valued params.
    ///
    /// Unlike the Prometheus API, **Alertmanager v2 returns bare JSON** — an
    /// array or object, with no `{"status": "success", "data": ...}` envelope.
    /// There is therefore nothing to unwrap here; copying Prometheus's `data`
    /// extraction would turn every successful call into `null`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, the HTTP status is not success, or
    /// the body is not valid JSON.
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
        let body = ensure_success(resp, &url).await?;

        serde_json::from_str(&body).wrap_err_with(|| format!("invalid JSON from {url}"))
    }

    /// `POST {base_url}{path}` with a JSON body, returning the parsed JSON
    /// response.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, the HTTP status is not success, or
    /// the body is not valid JSON.
    pub async fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);

        let resp = self
            .http
            .post(&url)
            .json(body)
            .send()
            .await
            .wrap_err_with(|| format!("request to {url} failed"))?;
        let body = ensure_success(resp, &url).await?;

        serde_json::from_str(&body).wrap_err_with(|| format!("invalid JSON from {url}"))
    }

    /// `DELETE {base_url}{path}`, discarding the body.
    ///
    /// Alertmanager answers a silence expiry with `200` and an empty body, so
    /// there is deliberately nothing to parse — attempting to would turn a
    /// success into a JSON error.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the HTTP status is not success.
    pub async fn delete(&self, path: &str) -> Result<()> {
        let url = format!("{}{}", self.base_url, path);

        let resp = self
            .http
            .delete(&url)
            .send()
            .await
            .wrap_err_with(|| format!("request to {url} failed"))?;
        ensure_success(resp, &url).await?;

        Ok(())
    }
}

/// Reject a non-2xx response before its body is parsed as if it were success,
/// returning the body text on success.
///
/// Alertmanager reports failures as a plain-text body (`"invalid silence ..."`)
/// or, behind a reverse proxy, as an HTML page — neither of which is the JSON the
/// caller expects. Parsing first produces one of two bad outcomes, both of which
/// were live bugs in this workspace's Home Assistant client:
///
/// * an opaque `error decoding response body` naming neither status nor cause; or
/// * worse, a body that *does* parse into a defaulted value, so a failure is
///   reported as success.
///
/// The second is the dangerous one here: `create_silence` reporting a silence
/// that was never created would leave an operator believing alerts are suppressed
/// during a maintenance window when they are not.
async fn ensure_success(resp: reqwest::Response, url: &str) -> Result<String> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .unwrap_or_else(|e| format!("<failed to read body: {e}>"));

    if status.is_success() {
        return Ok(body);
    }

    // Truncate on a character boundary — `String::truncate` panics mid-codepoint,
    // and these are long-running daemons.
    let trimmed = body.trim();
    let mut shown: String = trimmed.chars().take(MAX_ERROR_BODY).collect();
    if shown.chars().count() < trimmed.chars().count() {
        shown.push_str("... (truncated)");
    }
    if shown.is_empty() {
        shown.push_str("<empty body>");
    }

    bail!("Alertmanager API {url} returned {status}: {shown}");
}
