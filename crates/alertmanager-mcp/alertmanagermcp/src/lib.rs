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

use std::sync::Arc;

use color_eyre::eyre::{Result, WrapErr};
use mcp_common::health_router;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

/// Build an [`AlertmanagerServer`] from the environment and serve it over
/// streamable HTTP (mounted at `/mcp`) until the process is stopped.
///
/// # Errors
///
/// Returns an error if configuration is missing/invalid, the HTTP client cannot
/// be built, the bind address is unavailable, or the server fails while running.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;
    tracing::info!(
        base_url = %config.base_url,
        allow_silence = config.allow_silence,
        "starting alertmanagermcp"
    );
    if config.allow_silence {
        // Worth a line in the log on its own: this is the only state in which
        // this server can change what the homelab notifies about.
        tracing::warn!(
            "ALERTMANAGER_ALLOW_SILENCE is set — create_silence and expire_silence are registered"
        );
    }

    let client = AlertmanagerClient::new(&config)?;
    let allow_silence = config.allow_silence;

    let mut http_config = StreamableHttpServerConfig::default();
    if let Some(hosts) = config.allowed_hosts.clone() {
        http_config = http_config.with_allowed_hosts(hosts);
    }

    let service = StreamableHttpService::new(
        move || Ok(AlertmanagerServer::new(client.clone(), allow_silence)),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );

    let app = axum::Router::new()
        .nest_service("/mcp", service)
        .merge(health_router());

    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .wrap_err_with(|| format!("failed to bind {}", config.bind))?;
    tracing::info!(
        bind = %config.bind,
        "alertmanagermcp listening on http://{}/mcp",
        config.bind
    );

    axum::serve(listener, app)
        .await
        .wrap_err("streamable-HTTP server error")?;
    Ok(())
}
