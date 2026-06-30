//! Runtime configuration, sourced from environment variables.

use color_eyre::eyre::{Result, WrapErr};

/// Connection settings for a Prometheus instance.
#[derive(Clone, Debug)]
pub struct Config {
    /// Base URL including scheme and port, e.g. `http://prometheus.lan:9090`.
    pub base_url: String,
    /// Optional bearer token, sent as `Authorization: Bearer <token>` — set this
    /// when Prometheus sits behind an authenticating reverse proxy. `None` sends
    /// no auth header (the common case for an unauthenticated internal instance).
    pub token: Option<String>,
    /// Accept self-signed / invalid TLS certificates.
    pub insecure: bool,
    /// Address to bind the streamable-HTTP MCP server to (default `127.0.0.1:8082`).
    pub bind: String,
    /// Allowed `Host` header values for inbound requests. `None` keeps rmcp's
    /// loopback-only default (DNS-rebinding protection); set this when serving
    /// on a hostname (e.g. behind Tailscale or a reverse proxy).
    pub allowed_hosts: Option<Vec<String>>,
}

impl Config {
    /// Build a [`Config`] from the `PROM_*` environment variables.
    ///
    /// Required: `PROM_HOST`.
    /// Optional: `PROM_TOKEN`, `PROM_INSECURE` (`1`/`true`), `PROM_BIND`
    /// (default `127.0.0.1:8082`), `PROM_ALLOWED_HOSTS` (comma-separated).
    ///
    /// # Errors
    ///
    /// Returns an error if `PROM_HOST` is unset.
    pub fn from_env() -> Result<Self> {
        let host = std::env::var("PROM_HOST").wrap_err(
            "PROM_HOST must be set (e.g. http://prometheus.lan:9090 or prometheus.lan)",
        )?;
        let token = std::env::var("PROM_TOKEN").ok().filter(|s| !s.is_empty());
        let insecure = matches!(
            std::env::var("PROM_INSECURE").as_deref(),
            Ok("1" | "true" | "yes")
        );
        let bind = std::env::var("PROM_BIND").unwrap_or_else(|_| "127.0.0.1:8082".to_string());
        let allowed_hosts = std::env::var("PROM_ALLOWED_HOSTS").ok().map(|s| {
            s.split(',')
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .collect()
        });

        Ok(Self {
            base_url: normalize_base_url(&host),
            token,
            insecure,
            bind,
            allowed_hosts,
        })
    }
}

/// Turn a user-supplied host into a full base URL, defaulting scheme to `http`
/// and port to Prometheus's `9090` when not already specified.
fn normalize_base_url(host: &str) -> String {
    let h = host.trim().trim_end_matches('/');
    if h.starts_with("http://") || h.starts_with("https://") {
        h.to_string()
    } else if h.contains(':') {
        format!("http://{h}")
    } else {
        format!("http://{h}:9090")
    }
}
