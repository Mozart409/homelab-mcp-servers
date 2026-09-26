//! Woodpecker CI MCP server library.
//!
//! Provides a read-only async client ([`WpClient`]) for the Woodpecker CI REST API
//! and an MCP server ([`WpServer`]) exposing operational tools (repos, pipelines, logs,
//! agents, queue status) over a streamable-HTTP transport (served with axum, mounted
//! at `/mcp`). No tool can modify CI state; inspection only. Secrets and registries
//! are deliberately not exposed.

mod client;
mod config;
mod server;

pub use client::WpClient;
pub use config::Config;
pub use server::WpServer;

use color_eyre::eyre::Result;

/// Build the complete HTTP app for `config`: a [`WpServer`] per MCP session at
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
    let client = WpClient::new(config)?;
    let max_log_lines = config.max_log_lines;
    Ok(mcp_common::mcp_router(
        move || Ok(WpServer::new(client.clone(), max_log_lines)),
        config.allowed_hosts.clone(),
    ))
}

/// Build a [`WpServer`] from the environment and serve it over
/// streamable HTTP (mounted at `/mcp`) until the process is stopped.
///
/// # Errors
///
/// Returns an error if configuration is missing/invalid, the HTTP client cannot be built,
/// the bind address is unavailable, or the server fails while running.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;
    tracing::info!(base_url = %config.base_url, "starting wpmcp");

    let app = router(&config)?;
    mcp_common::serve(&config.bind, app, "wpmcp").await
}
