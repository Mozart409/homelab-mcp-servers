//! Runtime configuration, sourced from environment variables.

use color_eyre::eyre::{Result, WrapErr};

/// Connection settings for a Grafana Loki instance.
#[derive(Clone, Debug)]
pub struct Config {
    /// Base URL including scheme and port, e.g. `http://loki.lan:3100`.
    pub base_url: String,
    /// Optional bearer token, sent as `Authorization: Bearer <token>` — set this
    /// when Loki sits behind an authenticating reverse proxy. `None` sends no
    /// auth header.
    pub token: Option<String>,
    /// Optional tenant ID, sent as the `X-Scope-OrgID` header. Required when Loki
    /// runs in multi-tenant mode; `None` omits the header (single-tenant default).
    pub org_id: Option<String>,
    /// Accept self-signed / invalid TLS certificates.
    pub insecure: bool,
    /// Address to bind the streamable-HTTP MCP server to (default `127.0.0.1:8083`).
    pub bind: String,
    /// Allowed `Host` header values for inbound requests. `None` keeps rmcp's
    /// loopback-only default (DNS-rebinding protection); set this when serving
    /// on a hostname (e.g. behind Tailscale or a reverse proxy).
    pub allowed_hosts: Option<Vec<String>>,
}

impl Config {
    /// Build a [`Config`] from the `LOKI_*` environment variables.
    ///
    /// Required: `LOKI_HOST`.
    /// Optional: `LOKI_TOKEN`, `LOKI_ORG_ID`, `LOKI_INSECURE` (`1`/`true`),
    /// `LOKI_BIND` (default `127.0.0.1:8083`), `LOKI_ALLOWED_HOSTS`
    /// (comma-separated).
    ///
    /// # Errors
    ///
    /// Returns an error if `LOKI_HOST` is unset.
    pub fn from_env() -> Result<Self> {
        let host = std::env::var("LOKI_HOST")
            .wrap_err("LOKI_HOST must be set (e.g. http://loki.lan:3100 or loki.lan)")?;
        let token = std::env::var("LOKI_TOKEN").ok().filter(|s| !s.is_empty());
        let org_id = std::env::var("LOKI_ORG_ID").ok().filter(|s| !s.is_empty());
        let insecure = matches!(
            std::env::var("LOKI_INSECURE").as_deref(),
            Ok("1" | "true" | "yes")
        );
        let bind = std::env::var("LOKI_BIND").unwrap_or_else(|_| "127.0.0.1:8083".to_string());
        let allowed_hosts = std::env::var("LOKI_ALLOWED_HOSTS").ok().map(|s| {
            s.split(',')
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .collect()
        });

        Ok(Self {
            base_url: normalize_base_url(&host),
            token,
            org_id,
            insecure,
            bind,
            allowed_hosts,
        })
    }
}

/// Turn a user-supplied host into a full base URL, defaulting scheme to `http`
/// and port to Loki's `3100` when not already specified.
fn normalize_base_url(host: &str) -> String {
    let h = host.trim().trim_end_matches('/');
    if h.starts_with("http://") || h.starts_with("https://") {
        h.to_string()
    } else if h.contains(':') {
        format!("http://{h}")
    } else {
        format!("http://{h}:3100")
    }
}
