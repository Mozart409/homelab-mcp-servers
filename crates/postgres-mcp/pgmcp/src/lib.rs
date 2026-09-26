//! Postgres MCP server library.
//!
//! Provides a connection-pool wrapper ([`PgClient`]) over a Postgres instance
//! and an MCP server ([`PgServer`]) exposing read-only introspection and query
//! tools over a streamable-HTTP transport (served with axum, mounted at `/mcp`).

mod client;
mod config;
mod server;

pub use client::PgClient;
pub use config::Config;
pub use server::PgServer;

use color_eyre::eyre::Result;

/// Build the complete HTTP app for `config`: a [`PgServer`] per MCP session at
/// `/mcp`, plus the health routes.
///
/// This is what [`run`] serves, and what the end-to-end tests in `tests/` bind
/// on an ephemeral port — so the tests exercise the production router,
/// DNS-rebinding allow-list included, rather than a look-alike.
///
/// # Errors
///
/// Returns an error if the connection pool cannot be built.
pub fn router(config: &Config) -> Result<axum::Router> {
    let client = PgClient::new(config)?;
    Ok(mcp_common::mcp_router(
        move || Ok(PgServer::new(client.clone())),
        config.allowed_hosts.clone(),
    ))
}

/// Build a [`PgServer`] from the environment and serve it over
/// streamable HTTP (mounted at `/mcp`) until the process is stopped.
///
/// # Errors
///
/// Returns an error if configuration is missing/invalid, the connection pool cannot be built,
/// the bind address is unavailable, or the server fails while running.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;
    tracing::info!(bind = %config.bind, "starting pgmcp");

    let app = router(&config)?;
    mcp_common::serve(&config.bind, app, "pgmcp").await
}
