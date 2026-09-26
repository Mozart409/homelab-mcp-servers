//! Thin async HTTP client for the Proxmox Backup Server REST API.

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

/// Authenticated client for a single PBS instance.
///
/// Cheap to clone (wraps an `Arc` internally via [`reqwest::Client`]).
#[derive(Clone)]
pub struct PbsClient {
    http: reqwest::Client,
    base_url: String,
    /// Node name for `/nodes/{node}/...` endpoints.
    pub node: String,
}

impl PbsClient {
    /// Construct a client, baking the API-token `Authorization` header into the
    /// underlying [`reqwest::Client`].
    ///
    /// # Errors
    ///
    /// Returns an error if the token contains invalid header characters or the
    /// HTTP client cannot be built.
    pub fn new(config: &Config) -> Result<Self> {
        let mut headers = HeaderMap::new();
        // PBS expects: `Authorization: PBSAPIToken=TOKENID:TOKENSECRET`
        let auth = format!("PBSAPIToken={}", config.api_key);
        let mut auth_val = HeaderValue::from_str(&auth)
            .wrap_err("PBS_API_KEY contains invalid header characters")?;
        auth_val.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth_val);

        // rustls has no default provider in this workspace; see
        // `mcp_common::install_crypto_provider`.
        mcp_common::install_crypto_provider();

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .danger_accept_invalid_certs(config.insecure)
            .user_agent(concat!("pbsmcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .wrap_err("failed to build HTTP client")?;

        Ok(Self {
            http,
            base_url: config.base_url.clone(),
            node: config.node.clone(),
        })
    }

    /// `GET /api2/json{path}` with optional query params, returning the
    /// unwrapped `data` field of the PBS response envelope.
    ///
    /// This is what most tools want: PBS wraps payloads as `{ "data": ... }`,
    /// and the wrapper carries nothing they need. Use
    /// [`get_envelope`](Self::get_envelope) instead when an endpoint puts
    /// meaningful metadata next to `data` (e.g. the task-log endpoint reports
    /// the whole log's line count as `total`).
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, the response status is not
    /// success, or the body is not valid JSON.
    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let mut json = self.get_json(path, query).await?;
        // PBS wraps payloads as `{ "data": ... }`; unwrap when present.
        Ok(json.get_mut("data").map(Value::take).unwrap_or(json))
    }

    /// `GET /api2/json{path}` with optional query params, returning the FULL
    /// PBS response envelope rather than just its `data` field.
    ///
    /// PBS responses are shaped `{ "data": ..., "total": N, ... }`; the
    /// envelope siblings are endpoint-specific. Only paging-aware tools (e.g.
    /// `task_log`) should call this — everything else should use
    /// [`get`](Self::get) so their output shape stays the bare payload.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, the response status is not
    /// success, or the body is not valid JSON.
    pub async fn get_envelope(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        self.get_json(path, query).await
    }

    /// Perform the actual `GET /api2/json{path}` request and parse the body
    /// as JSON, returning the full response envelope untouched.
    ///
    /// This is the single place that builds the URL, sends the request, and
    /// validates/parses the response; [`get`](Self::get) and
    /// [`get_envelope`](Self::get_envelope) only differ in how much of the
    /// envelope they hand back to the caller.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, the response status is not
    /// success, or the body is not valid JSON.
    async fn get_json(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let url = format!("{}/api2/json{}", self.base_url, path);
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
            bail!("PBS API {url} returned {status}: {}", body.trim());
        }

        serde_json::from_str(&body).wrap_err_with(|| format!("invalid JSON from {url}"))
    }
}
