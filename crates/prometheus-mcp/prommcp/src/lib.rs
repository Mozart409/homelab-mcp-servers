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

use std::sync::Arc;

use color_eyre::eyre::{Result, WrapErr};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

/// Build a [`PromServer`] from the environment and serve it over streamable HTTP
/// (mounted at `/mcp`) until the process is stopped.
///
/// # Errors
///
/// Returns an error if configuration is missing/invalid, the HTTP client cannot
/// be built, the bind address is unavailable, or the server fails while running.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;
    tracing::info!(base_url = %config.base_url, "starting prommcp");

    let client = PromClient::new(&config)?;

    let mut http_config = StreamableHttpServerConfig::default();
    if let Some(hosts) = config.allowed_hosts.clone() {
        http_config = http_config.with_allowed_hosts(hosts);
    }

    let service = StreamableHttpService::new(
        move || Ok(PromServer::new(client.clone())),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );

    let app = axum::Router::new().nest_service("/mcp", service);

    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .wrap_err_with(|| format!("failed to bind {}", config.bind))?;
    tracing::info!(bind = %config.bind, "prommcp listening on http://{}/mcp", config.bind);

    axum::serve(listener, app)
        .await
        .wrap_err("streamable-HTTP server error")?;
    Ok(())
}
