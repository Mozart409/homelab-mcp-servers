//! Alertmanager MCP server library.
//!
//! Provides a small async client ([`AlertmanagerClient`]) for the Alertmanager
//! v2 HTTP API and an MCP server ([`AlertmanagerServer`]) exposing alert,
//! routing, and silence inspection tools over a streamable-HTTP transport
//! (served with axum, mounted at `/mcp`).
//!
//! Six tools are read-only and always available. Two — `create_silence` and
//! `expire_silence` — mutate the target and are registered only when
//! `ALERTMANAGER_ALLOW_SILENCE` is set; see [`Config::allow_silence`].

mod client;
mod config;
mod server;

pub use client::AlertmanagerClient;
pub use config::Config;
pub use server::AlertmanagerServer;

use color_eyre::eyre::Result;

/// Build the complete HTTP app for `config`: a [`AlertmanagerServer`] per MCP session at
/// `/mcp`, plus the health routes.
///
/// This is what [`run`] serves, and what the end-to-end tests in `tests/` bind
/// on an ephemeral port — so the tests exercise the production router,
/// DNS-rebinding allow-list included, rather than a look-alike.
///
/// # Errors
///
/// Returns an error if the HTTP client cannot be built.
pub fn router(config: &Config) -> Result<axum::Router> {
    let client = AlertmanagerClient::new(config)?;
    let allow_silence = config.allow_silence;
    Ok(mcp_common::mcp_router(
        move || Ok(AlertmanagerServer::new(client.clone(), allow_silence)),
        config.allowed_hosts.clone(),
    ))
}

/// Build an [`AlertmanagerServer`] from the environment and serve it over
/// streamable HTTP (mounted at `/mcp`) until the process is stopped.
///
/// # Errors
///
/// Returns an error if configuration is missing/invalid, the HTTP client cannot be built,
/// the bind address is unavailable, or the server fails while running.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;
    tracing::info!(
        base_url = %config.base_url,
        allow_silence = config.allow_silence,
        "starting alertmanagermcp"
    );
    if config.allow_silence {
        tracing::warn!(
            "ALERTMANAGER_ALLOW_SILENCE is set — create_silence and expire_silence are registered"
        );
    }

    let app = router(&config)?;
    mcp_common::serve(&config.bind, app, "alertmanagermcp").await
}
