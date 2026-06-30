//! Thin async HTTP client for the Proxmox Backup Server REST API.

use color_eyre::eyre::{Result, WrapErr, bail};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;

use crate::config::Config;

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
    /// # Errors
    ///
    /// Returns an error if the request fails, the response status is not
    /// success, or the body is not valid JSON.
    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
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

        let mut json: Value =
            serde_json::from_str(&body).wrap_err_with(|| format!("invalid JSON from {url}"))?;
        // PBS wraps payloads as `{ "data": ... }`; unwrap when present.
        Ok(json.get_mut("data").map(Value::take).unwrap_or(json))
    }
}
