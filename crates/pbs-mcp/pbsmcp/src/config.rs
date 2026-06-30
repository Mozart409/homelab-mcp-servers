//! Runtime configuration, sourced from environment variables.

use color_eyre::eyre::{Result, WrapErr};

/// Connection settings for a Proxmox Backup Server instance.
#[derive(Clone, Debug)]
pub struct Config {
    /// Base URL including scheme and port, e.g. `https://pbs.lan:8007`.
    pub base_url: String,
    /// API token, formatted `user@realm!tokenname:secret` (the value sent after
    /// `PBSAPIToken=`).
    pub api_key: String,
    /// PBS node name used for `/nodes/{node}/...` endpoints (default `localhost`).
    pub node: String,
    /// Accept self-signed / invalid TLS certificates.
    pub insecure: bool,
    /// Address to bind the streamable-HTTP MCP server to (default `127.0.0.1:8080`).
    pub bind: String,
    /// Allowed `Host` header values for inbound requests. `None` keeps rmcp's
    /// loopback-only default (DNS-rebinding protection); set this when serving
    /// on a hostname (e.g. behind Tailscale or a reverse proxy).
    pub allowed_hosts: Option<Vec<String>>,
}

impl Config {
    /// Build a [`Config`] from the `PBS_*` environment variables.
    ///
    /// Required: `PBS_HOST`, `PBS_API_KEY`.
    /// Optional: `PBS_NODE` (default `localhost`), `PBS_INSECURE` (`1`/`true`),
    /// `PBS_BIND` (default `127.0.0.1:8080`), `PBS_ALLOWED_HOSTS` (comma-separated).
    ///
    /// # Errors
    ///
    /// Returns an error if any required environment variable is unset.
    pub fn from_env() -> Result<Self> {
        let host = std::env::var("PBS_HOST")
            .wrap_err("PBS_HOST must be set (e.g. https://pbs.lan:8007 or pbs.lan)")?;
        let api_key = std::env::var("PBS_API_KEY")
            .wrap_err("PBS_API_KEY must be set (e.g. user@pbs!tokenname:secret)")?;
        let node = std::env::var("PBS_NODE").unwrap_or_else(|_| "localhost".to_string());
        let insecure = matches!(
            std::env::var("PBS_INSECURE").as_deref(),
            Ok("1" | "true" | "yes")
        );
        let bind = std::env::var("PBS_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
        let allowed_hosts = std::env::var("PBS_ALLOWED_HOSTS").ok().map(|s| {
            s.split(',')
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .collect()
        });

        Ok(Self {
            base_url: normalize_base_url(&host),
            api_key,
            node,
            insecure,
            bind,
            allowed_hosts,
        })
    }
}

/// Turn a user-supplied host into a full base URL, defaulting scheme to
/// `https` and port to PBS's `8007` when not already specified.
fn normalize_base_url(host: &str) -> String {
    let h = host.trim().trim_end_matches('/');
    if h.starts_with("http://") || h.starts_with("https://") {
        h.to_string()
    } else if h.contains(':') {
        format!("https://{h}")
    } else {
        format!("https://{h}:8007")
    }
}
