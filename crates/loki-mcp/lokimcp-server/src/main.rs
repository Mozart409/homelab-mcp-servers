//! Loki MCP server entry point.

use color_eyre::eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
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

    lokimcp::run().await?;
    Ok(())
}
