//! Alertmanager MCP server entry point.

use std::env;

use color_eyre::eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
    if env::args().any(|a| a == "--healthcheck") {
        let bind = env::var("ALERTMANAGER_BIND").unwrap_or_else(|_| "127.0.0.1:8086".to_string());
        return mcp_common::run_healthcheck(&bind).await;
    }

    color_eyre::install()?;
    // Load a local `.env` if present; real environment variables take precedence.
    // Missing file is fine (e.g. when env is provided by the MCP client config).
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    alertmanagermcp::run().await?;
    Ok(())
}
