//! Environment-based configuration for the Home Assistant MCP server.
#![allow(clippy::missing_errors_doc)]

use std::env;

use color_eyre::eyre::{Context, Result};

/// Server configuration loaded from environment variables.
#[derive(Clone, Debug)]
pub struct Config {
    /// Base URL of the Home Assistant instance.
    pub base_url: String,
    /// Long-lived access token for authentication.
    pub token: String,
    /// Accept invalid TLS certificates (dangerous, for testing only).
    pub insecure: bool,
    /// Bind address for the MCP server.
    pub bind: String,
    /// Allowed Origin / Host values for DNS-rebinding protection.
    pub allowed_hosts: Option<Vec<String>>,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Required:
    /// - `HA_HOST` — Home Assistant instance host/URL
    /// - `HA_TOKEN` — long-lived access token
    ///
    /// Optional:
    /// - `HA_INSECURE` — `1`/`true`/`yes` to skip TLS verification (default: off)
    /// - `HA_BIND` — bind address (default: `127.0.0.1:8084`)
    /// - `HA_ALLOWED_HOSTS` — comma-separated allowed hosts
    pub fn from_env() -> Result<Self> {
        let host = env::var("HA_HOST").wrap_err("HA_HOST environment variable is required")?;
        let token = env::var("HA_TOKEN").wrap_err("HA_TOKEN environment variable is required")?;

        // `1`/`true`/`yes`, as the README documents and the other servers accept.
        // (This used to honour only `true`, so the README's `HA_INSECURE=1` was
        // silently ignored, on top of the flag never reaching the client.)
        let insecure = env::var("HA_INSECURE")
            .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"));

        let bind = env::var("HA_BIND").unwrap_or_else(|_| "127.0.0.1:8084".to_string());

        let allowed_hosts = env::var("HA_ALLOWED_HOSTS")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect::<Vec<String>>()
            })
            .filter(|v| !v.is_empty());

        Ok(Self {
            base_url: normalize_base_url(&host),
            token,
            insecure,
            bind,
            allowed_hosts,
        })
    }
}

/// Ensure the host has a scheme, defaulting to `http://` for local Home Assistant.
fn normalize_base_url(host: &str) -> String {
    if host.starts_with("http://") || host.starts_with("https://") {
        host.to_string()
    } else {
        format!("http://{host}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_base_url_with_scheme() {
        assert_eq!(
            normalize_base_url("http://ha.local:8123"),
            "http://ha.local:8123"
        );
        assert_eq!(
            normalize_base_url("https://ha.local:8123"),
            "https://ha.local:8123"
        );
    }

    #[test]
    fn test_normalize_base_url_without_scheme() {
        assert_eq!(normalize_base_url("ha.local:8123"), "http://ha.local:8123");
    }
}
