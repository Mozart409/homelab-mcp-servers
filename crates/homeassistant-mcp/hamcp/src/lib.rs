//! Home Assistant MCP server library.
//!
//! Provides an async REST client ([`HaClient`]) for a Home Assistant instance
//! and an MCP server ([`HaServer`]) exposing smart-home control and query tools
//! over a streamable-HTTP transport (served with axum, mounted at `/mcp`).

mod client;
mod config;
mod server;

pub mod models;

pub use client::{ClientError, HaClient, Result as ClientResult};
pub use config::Config;
pub use server::HaServer;

use color_eyre::eyre::{Result, WrapErr};

/// Build the complete HTTP app for `config`: a [`HaServer`] per MCP session at
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
    let client = HaClient::new(&config.base_url, &config.token, config.insecure)
        .wrap_err("failed to create Home Assistant client")?;
    Ok(mcp_common::mcp_router(
        move || Ok(HaServer::new(client.clone())),
        config.allowed_hosts.clone(),
    ))
}

/// Build a [`HaServer`] from the environment and serve it over
/// streamable HTTP (mounted at `/mcp`) until the process is stopped.
///
/// # Errors
///
/// Returns an error if configuration is missing/invalid, the HTTP client cannot be built,
/// the bind address is unavailable, or the server fails while running.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;
    tracing::info!(base_url = %config.base_url, "starting hamcp");

    let app = router(&config)?;
    mcp_common::serve(&config.bind, app, "hamcp").await
}
