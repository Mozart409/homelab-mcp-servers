//! Shared MCP server plumbing for the homelab-mcp-servers monorepo.
//!
//! Provides cross-server conventions that would otherwise be copy-pasted into
//! every crate, starting with the healthcheck probe and the `/_healthcheck` HTTP
//! route that the distroless containers rely on (there is no shell or curl).

use axum::{Json, Router, routing::get};
use color_eyre::eyre::{Context, Result};
use serde::Serialize;

/// Build an axum [`Router`] that mounts `/_healthcheck` and `/` GET routes.
///
/// Each MCP server calls this and merges the router into its own `run()` so that
/// the health endpoints are available alongside `/mcp`.
pub fn health_router() -> Router {
    Router::new()
        .route("/_healthcheck", get(health_handler))
        .route("/", get(health_handler))
}

/// Run a client-side healthcheck probe against a running server.
///
/// This is the entry point for the `--healthcheck` CLI flag that every server
/// binary supports. It must work **without** config/dotenv (the distroless
/// container has no shell and no `.env` file), so callers pass the bind address
/// they already know from the env/default.
///
/// Exits with code 0 on success, 1 on failure.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the server does not respond
/// with a success status.
pub async fn run_healthcheck(bind: &str) -> Result<()> {
    let url = format!("http://{bind}/_healthcheck");

    let response = reqwest::get(&url)
        .await
        .with_context(|| format!("health check request to {url} failed"))?;

    if response.status().is_success() {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

// ---- Response types ---------------------------------------------------------

/// Health check response body.
#[derive(Serialize)]
struct HealthcheckResponse {
    status: &'static str,
}

/// Simple health check handler for container probes.
async fn health_handler() -> Json<HealthcheckResponse> {
    Json(HealthcheckResponse { status: "ok" })
}

// ---- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_router_is_router() {
        let r = health_router();
        // Smoke: it builds and returns a Router.
        drop(r);
    }

    #[tokio::test]
    async fn test_health_handler_returns_ok() {
        let Json(resp) = health_handler().await;
        assert_eq!(resp.status, "ok");
    }
}
