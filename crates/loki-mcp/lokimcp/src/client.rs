//! Thin async HTTP client for the Grafana Loki HTTP API.

use color_eyre::eyre::{Result, WrapErr, bail};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use crate::config::Config;

/// Percent-encode a single URL path segment.
///
/// Loki label names may contain reserved characters (e.g. `:`); interpolating
/// them raw would change the URL's structure.
pub(crate) fn seg(s: &str) -> impl std::fmt::Display + '_ {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC)
}

/// Client for a single Loki instance.
///
/// Cheap to clone (wraps an `Arc` internally via [`reqwest::Client`]).
#[derive(Clone)]
pub struct LokiClient {
    http: reqwest::Client,
    base_url: String,
}

impl LokiClient {
    /// Construct a client, baking an optional bearer-token `Authorization` header
    /// and an optional `X-Scope-OrgID` tenant header into the underlying
    /// [`reqwest::Client`].
    ///
    /// # Errors
    ///
    /// Returns an error if a header value contains invalid characters or the HTTP
    /// client cannot be built.
    pub fn new(config: &Config) -> Result<Self> {
        let mut headers = HeaderMap::new();
        if let Some(token) = &config.token {
            let mut auth_val = HeaderValue::from_str(&format!("Bearer {token}"))
                .wrap_err("LOKI_TOKEN contains invalid header characters")?;
            auth_val.set_sensitive(true);
            headers.insert(AUTHORIZATION, auth_val);
        }
        if let Some(org_id) = &config.org_id {
            let val = HeaderValue::from_str(org_id)
                .wrap_err("LOKI_ORG_ID contains invalid characters")?;
            headers.insert(HeaderName::from_static("x-scope-orgid"), val);
        }

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .danger_accept_invalid_certs(config.insecure)
            .user_agent(concat!("lokimcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .wrap_err("failed to build HTTP client")?;

        Ok(Self {
            http,
            base_url: config.base_url.clone(),
        })
    }

    /// `GET {base_url}{path}` with optional query params, returning the unwrapped
    /// `data` field of the Loki response envelope. Query keys may repeat (e.g.
    /// `match[]`), which is how Loki expects multi-valued params.
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

        if !status.is_success() {
            bail!("Loki API {url} returned {status}: {}", body.trim());
        }

        let mut json: Value =
            serde_json::from_str(&body).wrap_err_with(|| format!("invalid JSON from {url}"))?;
        if json.get("status").and_then(Value::as_str) == Some("error") {
            let msg = json
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("Loki API {url} error: {msg}");
        }

        // Loki wraps query/label payloads as `{ "status": "success", "data": ... }`.
        Ok(json.get_mut("data").map(Value::take).unwrap_or(json))
    }
}

#[cfg(test)]
mod tests {
    use super::seg;

    #[test]
    fn seg_encodes_colon_in_label_name() {
        // Loki label names may legally contain `:`; the raw colon must not
        // survive into the URL path.
        assert_eq!(seg("foo:bar").to_string(), "foo%3Abar");
    }

    #[test]
    fn seg_passes_alphanumeric_through() {
        assert_eq!(seg("job").to_string(), "job");
    }
}
