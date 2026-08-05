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

use std::sync::Arc;

use color_eyre::eyre::{Result, WrapErr};
use mcp_common::health_router;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

/// Build a [`WpServer`] from the environment and serve it over streamable HTTP
/// (mounted at `/mcp`) until the process is stopped.
///
/// Reads `WP_*` env vars via [`Config::from_env()`], logs startup info, builds
/// the HTTP client to Woodpecker, and serves tools over streamable HTTP. A successful
/// call means the server is listening and ready to accept MCP requests.
///
/// # Errors
///
/// Returns an error if configuration is missing/invalid (e.g. `WP_HOST` or
/// `WP_TOKEN` unset), the HTTP client cannot be built, the bind address is
/// unavailable, or the server fails while running.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;
    tracing::info!(base_url = %config.base_url, "starting wpmcp");

    let client = WpClient::new(&config)?;
    let max_log_lines = config.max_log_lines;

    let mut http_config = StreamableHttpServerConfig::default();
    if let Some(hosts) = config.allowed_hosts.clone() {
        http_config = http_config.with_allowed_hosts(hosts);
    }

    let service = StreamableHttpService::new(
        move || Ok(WpServer::new(client.clone(), max_log_lines)),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );

    let app = axum::Router::new()
        .nest_service("/mcp", service)
        .merge(health_router());

    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .wrap_err_with(|| format!("failed to bind {}", config.bind))?;
    tracing::info!(bind = %config.bind, "wpmcp listening on http://{}/mcp", config.bind);

    axum::serve(listener, app)
        .await
        .wrap_err("streamable-HTTP server error")?;
    Ok(())
}
