//! Tempo MCP server library.
//!
//! Provides a small async client ([`TempoClient`]) for the Grafana Tempo HTTP
//! API and an MCP server ([`TempoServer`]) exposing read-only trace lookup and
//! `TraceQL` search tools over a streamable-HTTP transport (served with axum,
//! mounted at `/mcp`).
//!
//! The scope is deliberately **individual traces and `TraceQL` search**. The
//! RED view of the same spans (rate, errors, duration) is already answerable
//! through prometheus-mcp, because Tempo's metrics-generator remote-writes
//! span-metrics and service-graph series into Prometheus; this crate does not
//! reimplement that aggregation.
//!
//! The one real design problem is response size. A trace is an OTLP document
//! carrying every span, attribute and event, and it is headed for an LLM's
//! context window. The `compact` module is where that is handled: `trace`
//! answers with a span tree cut to what a reader needs, and `search` with
//! trace summaries, never spans. The full document is an explicit opt-in.

mod client;
mod compact;
mod config;
mod server;
mod time;

pub use client::TempoClient;
pub use config::Config;
pub use server::TempoServer;

use color_eyre::eyre::Result;

/// Build the complete HTTP app for `config`: a [`TempoServer`] per MCP session
/// at `/mcp`, plus the health routes.
///
/// This is what [`run`] serves, and what the end-to-end tests in `tests/` bind
/// on an ephemeral port — so the tests exercise the production router,
/// DNS-rebinding allow-list included, rather than a look-alike.
///
/// # Errors
///
/// Returns an error if the HTTP client cannot be built.
pub fn router(config: &Config) -> Result<axum::Router> {
    let client = TempoClient::new(config)?;
    Ok(mcp_common::mcp_router(
        move || Ok(TempoServer::new(client.clone())),
        config.allowed_hosts.clone(),
    ))
}

/// Build a [`TempoServer`] from the environment and serve it over
/// streamable HTTP (mounted at `/mcp`) until the process is stopped.
///
/// # Errors
///
/// Returns an error if configuration is missing/invalid, the HTTP client cannot be built,
/// the bind address is unavailable, or the server fails while running.
pub async fn run() -> Result<()> {
    let config = Config::from_env()?;
    tracing::info!(base_url = %config.base_url, "starting tempomcp");

    let app = router(&config)?;
    mcp_common::serve(&config.bind, app, "tempomcp").await
}
