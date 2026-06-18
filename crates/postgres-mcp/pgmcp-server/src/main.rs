//! Postgres MCP server entry point.

use color_eyre::eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    tracing::info!("pgmcp-server: not yet implemented");
    Ok(())
}
