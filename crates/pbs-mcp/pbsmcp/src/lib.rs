//! Proxmox Backup Server MCP server library.
//!
//! Provides a small async REST client ([`PbsClient`]) for a PBS instance and an
//! MCP server ([`PbsServer`]) exposing read-only backup-status tools over a
//! streamable-HTTP transport (served with axum, mounted at `/mcp`).

mod client;
mod config;
mod server;

pub use client::PbsClient;
pub use config::Config;
pub use server::PbsServer;

use color_eyre::eyre::Result;

/// Build the complete HTTP app for `config`: a [`PbsServer`] per MCP session at
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
    let client = PbsClient::new(config)?;
    Ok(mcp_common::mcp_router(
        move || Ok(PbsServer::new(client.clone())),
        config.allowed_hosts.clone(),
    ))
}

/// Build a [`PbsServer`] from the environment and serve it over
/// streamable HTTP (mounted at `/mcp`) until the process is stopped.
///
/// # Errors
///
/// Returns an error if configuration is missing/invalid, the HTTP client cannot be built,
/// the bind address is unavailable, or the server fails while running.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;
    tracing::info!(node = %config.node, base_url = %config.base_url, "starting pbsmcp");

    let app = router(&config)?;
    mcp_common::serve(&config.bind, app, "pbsmcp").await
}
