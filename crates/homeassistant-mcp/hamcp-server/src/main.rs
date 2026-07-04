//! Home Assistant MCP server entry point.

use std::env;

use color_eyre::eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Healthcheck runs before config/dotenv so it works in distroless.
    if env::args().any(|a| a == "--healthcheck") {
        let bind = env::var("HA_BIND").unwrap_or_else(|_| "127.0.0.1:8084".to_string());
        return mcp_common::run_healthcheck(&bind).await;
    }

    color_eyre::install()?;
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    hamcp::run().await
}
