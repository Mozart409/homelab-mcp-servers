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

use std::sync::Arc;

use color_eyre::eyre::{Result, WrapErr};
use mcp_common::health_router;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

/// Build a [`PgServer`] from the environment and serve it over streamable HTTP
/// (mounted at `/mcp`) until the process is stopped.
///
/// # Errors
///
/// Returns an error if configuration is missing/invalid, the connection pool
/// cannot be built, the bind address is unavailable, or the server fails while
/// running.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;
    tracing::info!(bind = %config.bind, "starting pgmcp");

    let client = PgClient::new(&config)?;

    let mut http_config = StreamableHttpServerConfig::default();
    if let Some(hosts) = config.allowed_hosts.clone() {
        http_config = http_config.with_allowed_hosts(hosts);
    }

    let service = StreamableHttpService::new(
        move || Ok(PgServer::new(client.clone())),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );

    let app = axum::Router::new()
        .nest_service("/mcp", service)
        .merge(health_router());

    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .wrap_err_with(|| format!("failed to bind {}", config.bind))?;
    tracing::info!(bind = %config.bind, "pgmcp listening on http://{}/mcp", config.bind);

    axum::serve(listener, app)
        .await
        .wrap_err("streamable-HTTP server error")?;
    Ok(())
}
