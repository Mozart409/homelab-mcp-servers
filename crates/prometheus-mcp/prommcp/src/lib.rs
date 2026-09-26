//! Prometheus MCP server library.
//!
//! Provides a small async client ([`PromClient`]) for the Prometheus HTTP API
//! and an MCP server ([`PromServer`]) exposing read-only query and status tools
//! over a streamable-HTTP transport (served with axum, mounted at `/mcp`).

mod client;
mod config;
mod server;

pub use client::PromClient;
pub use config::Config;
pub use server::PromServer;

use color_eyre::eyre::Result;

/// Build the complete HTTP app for `config`: a [`PromServer`] per MCP session at
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
    let client = PromClient::new(config)?;
    Ok(mcp_common::mcp_router(
        move || Ok(PromServer::new(client.clone())),
        config.allowed_hosts.clone(),
    ))
}

/// Build a [`PromServer`] from the environment and serve it over
/// streamable HTTP (mounted at `/mcp`) until the process is stopped.
///
/// # Errors
///
/// Returns an error if configuration is missing/invalid, the HTTP client cannot be built,
/// the bind address is unavailable, or the server fails while running.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;
    tracing::info!(base_url = %config.base_url, "starting prommcp");

    let app = router(&config)?;
    mcp_common::serve(&config.bind, app, "prommcp").await
}
