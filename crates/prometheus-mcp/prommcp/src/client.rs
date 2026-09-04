//! Thin async HTTP client for the Prometheus HTTP API.

use color_eyre::eyre::{Result, WrapErr, bail};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;

use crate::config::Config;

/// Percent-encode a single URL path segment.
///
/// Interpolating caller-supplied label names raw would let reserved
/// characters change the URL's structure.
pub(crate) fn seg(s: &str) -> impl std::fmt::Display + '_ {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC)
}

/// Client for a single Prometheus instance.
///
/// Cheap to clone (wraps an `Arc` internally via [`reqwest::Client`]).
#[derive(Clone)]
pub struct PromClient {
    http: reqwest::Client,
    base_url: String,
}

impl PromClient {
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
                .wrap_err("PROM_TOKEN contains invalid header characters")?;
            auth_val.set_sensitive(true);
            headers.insert(AUTHORIZATION, auth_val);
        }

        // rustls has no default provider in this workspace; see
        // `mcp_common::install_crypto_provider`.
        mcp_common::install_crypto_provider();

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .danger_accept_invalid_certs(config.insecure)
            .user_agent(concat!("prommcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .wrap_err("failed to build HTTP client")?;

        Ok(Self {
            http,
            base_url: config.base_url.clone(),
        })
    }

    /// `GET {base_url}{path}` with optional query params, returning the unwrapped
    /// `data` field of the Prometheus response envelope. Query keys may repeat
    /// (e.g. `match[]`), which is how Prometheus expects multi-valued params.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, the HTTP status is not success, the
    /// body is not valid JSON, or the API reports `status: "error"`.
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

        // Prometheus returns a JSON error envelope even on 4xx/5xx; prefer that
        // message when the body parses, otherwise fall back to the raw status.
        let mut json: Value = serde_json::from_str(&body).map_err(|e| {
            if status.is_success() {
                color_eyre::eyre::eyre!("invalid JSON from {url}: {e}")
            } else {
                color_eyre::eyre::eyre!("Prometheus API {url} returned {status}: {}", body.trim())
            }
        })?;

        if json.get("status").and_then(Value::as_str) == Some("error") {
            let msg = json
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("Prometheus API {url} error: {msg}");
        }
        if !status.is_success() {
            bail!("Prometheus API {url} returned {status}: {}", body.trim());
        }

        // Prometheus wraps payloads as `{ "status": "success", "data": ... }`.
        Ok(json.get_mut("data").map(Value::take).unwrap_or(json))
    }
}

#[cfg(test)]
mod tests {
    use super::seg;

    #[test]
    fn seg_encodes_reserved_char() {
        // A label name containing `/` must not split into extra path segments.
        assert_eq!(seg("foo/bar").to_string(), "foo%2Fbar");
    }

    #[test]
    fn seg_passes_alphanumeric_through() {
        assert_eq!(seg("job").to_string(), "job");
    }
}
