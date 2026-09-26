//! Thin async HTTP client for the Grafana Tempo HTTP API.

use color_eyre::eyre::{Result, WrapErr, bail};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use crate::config::Config;

/// How much of a failed response's body goes into the error message.
///
/// Tempo's own errors are one plain-text line (`invalid TraceQL query: parse
/// error at line 1, col 3: …`), but a failure in front of it (the reverse
/// proxy's HTML error page, a load balancer's 502) can be kilobytes of markup.
/// The message is headed for an LLM's context, so it is cut here.
const MAX_ERROR_BODY: usize = 512;

/// Client for a single Tempo instance.
///
/// Cheap to clone (wraps an `Arc` internally via [`reqwest::Client`]).
#[derive(Clone)]
pub struct TempoClient {
    http: reqwest::Client,
    base_url: String,
}

impl TempoClient {
    /// Construct a client, baking an optional bearer-token `Authorization` header
    /// and an optional `X-Scope-OrgID` tenant header into the underlying
    /// [`reqwest::Client`].
    ///
    /// `Accept: application/json` is sent on every request: Tempo's trace
    /// endpoints can also answer in protobuf, and JSON is the only shape this
    /// crate parses.
    ///
    /// # Errors
    ///
    /// Returns an error if a header value contains invalid characters or the HTTP
    /// client cannot be built.
    pub fn new(config: &Config) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if let Some(token) = &config.token {
            let mut auth_val = HeaderValue::from_str(&format!("Bearer {token}"))
                .wrap_err("TEMPO_TOKEN contains invalid header characters")?;
            auth_val.set_sensitive(true);
            headers.insert(AUTHORIZATION, auth_val);
        }
        if let Some(org_id) = &config.org_id {
            let val = HeaderValue::from_str(org_id)
                .wrap_err("TEMPO_ORG_ID contains invalid characters")?;
            headers.insert(HeaderName::from_static("x-scope-orgid"), val);
        }

        // rustls has no default provider in this workspace; see
        // `mcp_common::install_crypto_provider`.
        mcp_common::install_crypto_provider();

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .danger_accept_invalid_certs(config.insecure)
            .user_agent(concat!("tempomcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .wrap_err("failed to build HTTP client")?;

        Ok(Self {
            http,
            base_url: config.base_url.clone(),
        })
    }

    /// `GET {base_url}{path}`, returning the status and body whatever the
    /// status was. Only a transport failure is an error here.
    async fn fetch(&self, path: &str, query: &[(&str, String)]) -> Result<(StatusCode, String)> {
        let url = self.url(path);
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
        Ok((status, body))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// `GET {base_url}{path}` with optional query params, parsed as JSON.
    ///
    /// Tempo, unlike Prometheus and Loki, does not wrap its answers in a
    /// `{status, data}` envelope, so the body is returned as-is.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, the HTTP status is not success, or
    /// the body is not valid JSON.
    pub async fn get_json(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let (status, body) = self.fetch(path, query).await?;
        let url = self.url(path);
        if !status.is_success() {
            bail!("Tempo API {url} returned {status}: {}", error_body(&body));
        }
        serde_json::from_str(&body).wrap_err_with(|| format!("invalid JSON from {url}"))
    }

    /// Like [`get_json`](Self::get_json), but a `404` is `Ok(None)` rather
    /// than an error, so the caller can say *why* a trace was not found
    /// instead of passing on Tempo's bare "trace not found".
    ///
    /// # Errors
    ///
    /// As [`get_json`](Self::get_json), for every status but `404`.
    pub async fn get_json_or_404(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Option<Value>> {
        let (status, body) = self.fetch(path, query).await?;
        let url = self.url(path);
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            bail!("Tempo API {url} returned {status}: {}", error_body(&body));
        }
        serde_json::from_str(&body)
            .map(Some)
            .wrap_err_with(|| format!("invalid JSON from {url}"))
    }

    /// `GET {base_url}{path}` for the plain-text endpoints (`/api/echo`,
    /// `/ready`), returning the trimmed body.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the HTTP status is not success.
    pub async fn get_text(&self, path: &str) -> Result<String> {
        let (status, body) = self.fetch(path, &[]).await?;
        if !status.is_success() {
            bail!(
                "Tempo API {} returned {status}: {}",
                self.url(path),
                error_body(&body)
            );
        }
        Ok(body.trim().to_string())
    }
}

/// A failed response's body, trimmed and cut to [`MAX_ERROR_BODY`] characters,
/// or a placeholder when it is empty (a proxy's bare `401` usually is), so the
/// message never ends in a dangling colon.
fn error_body(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return "(empty body)".to_string();
    }
    match body.char_indices().nth(MAX_ERROR_BODY) {
        Some((cut, _)) => format!(
            "{}… ({} bytes total)",
            body.get(..cut).unwrap_or(body),
            body.len()
        ),
        None => body.to_string(),
    }
}
