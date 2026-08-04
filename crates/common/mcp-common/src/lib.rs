//! Shared MCP server plumbing for the homelab-mcp-servers monorepo.
//!
//! Provides cross-server conventions that would otherwise be copy-pasted into
//! every crate, starting with the healthcheck probe and the `/_healthcheck` HTTP
//! route that the distroless containers rely on (there is no shell or curl).

use std::time::Duration;

use axum::{Json, Router, routing::get};
use color_eyre::eyre::{Context, Result, bail};
use serde::Serialize;

/// How long the `--healthcheck` probe waits for the server before giving up.
///
/// A container healthcheck that hangs is worse than one that fails: the
/// orchestrator learns nothing while the probe blocks. Bound it explicitly
/// rather than relying on `reqwest`'s (absent) default timeout.
const HEALTHCHECK_TIMEOUT: Duration = Duration::from_secs(5);

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
/// The probe is the whole body of `main` in that mode, so returning `Ok`
/// exits the process with code 0 and returning `Err` exits with code 1 (and
/// prints why) — which is exactly the contract a container healthcheck needs.
///
/// # Errors
///
/// Returns an error if the HTTP request fails (nothing listening, DNS, or the
/// [`HEALTHCHECK_TIMEOUT`] elapsing) or the server does not respond with a
/// success status.
pub async fn run_healthcheck(bind: &str) -> Result<()> {
    let url = format!("http://{bind}/_healthcheck");

    let client = reqwest::Client::builder()
        .timeout(HEALTHCHECK_TIMEOUT)
        .build()
        .wrap_err("failed to build health check HTTP client")?;

    let response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("health check request to {url} failed"))?;

    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        bail!("health check at {url} returned non-success status {status}");
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

    use axum::http::StatusCode;
    use serde_json::Value;
    use tokio::net::TcpListener;

    /// Serve `router` on an ephemeral loopback port and return its `host:port`.
    ///
    /// Binding `127.0.0.1:0` and reading the assigned port back keeps the tests
    /// free of hard-coded ports (and therefore runnable in parallel and in CI).
    /// The server task is detached; it dies with the test process.
    ///
    /// Helpers live outside any `#[test]` fn, where the workspace's
    /// `unwrap_used`/`expect_used` denies are *not* relaxed by `clippy.toml` —
    /// hence the `Result` return and `?` throughout.
    async fn serve(router: Router) -> Result<String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .wrap_err("failed to bind ephemeral test port")?;
        let addr = listener
            .local_addr()
            .wrap_err("failed to read assigned test port")?;

        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Ok(addr.to_string())
    }

    /// Reserve an ephemeral port and immediately release it, so a probe against
    /// the returned address is refused rather than answered.
    async fn dead_addr() -> Result<String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .wrap_err("failed to bind ephemeral test port")?;
        let addr = listener
            .local_addr()
            .wrap_err("failed to read assigned test port")?;
        drop(listener);
        Ok(addr.to_string())
    }

    /// GET `http://{addr}{path}` and return the decoded JSON body.
    async fn get_json(addr: &str, path: &str) -> Result<Value> {
        let url = format!("http://{addr}{path}");
        let response = reqwest::get(&url)
            .await
            .wrap_err_with(|| format!("GET {url} failed"))?;
        if !response.status().is_success() {
            bail!("GET {url} returned {}", response.status());
        }
        response
            .json()
            .await
            .wrap_err_with(|| format!("GET {url} returned a non-JSON body"))
    }

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

    #[tokio::test]
    async fn healthcheck_succeeds_against_the_real_health_router() {
        let addr = serve(health_router()).await.unwrap();

        run_healthcheck(&addr)
            .await
            .expect("probe against health_router() must succeed");
    }

    #[tokio::test]
    async fn healthcheck_errors_on_non_success_status() {
        // Same route, deliberately unhealthy: the probe must not treat a
        // reachable-but-failing server as healthy.
        let router = Router::new().route(
            "/_healthcheck",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        );
        let addr = serve(router).await.unwrap();

        let err = run_healthcheck(&addr)
            .await
            .expect_err("503 must be reported as a failure");
        assert!(
            format!("{err:#}").contains("503"),
            "error should mention the status: {err:#}"
        );
    }

    #[tokio::test]
    async fn healthcheck_errors_when_nothing_is_listening() {
        let addr = dead_addr().await.unwrap();

        // The probe must fail *and* return promptly: a container healthcheck
        // that blocks forever tells the orchestrator nothing.
        let res = tokio::time::timeout(Duration::from_secs(10), run_healthcheck(&addr))
            .await
            .expect("probe against a closed port must not hang");
        assert!(res.is_err(), "connection refused must surface as an error");
    }

    #[tokio::test]
    async fn both_health_routes_return_the_same_documented_body() {
        let addr = serve(health_router()).await.unwrap();

        let healthcheck = get_json(&addr, "/_healthcheck").await.unwrap();
        let root = get_json(&addr, "/").await.unwrap();

        // The container config may probe either path, so they must agree.
        assert_eq!(healthcheck, root, "/ and /_healthcheck must agree");

        // Pin the exact wire shape: renaming/adding a field on
        // `HealthcheckResponse` is a contract change and must fail here.
        assert_eq!(healthcheck, serde_json::json!({ "status": "ok" }));
        let obj = healthcheck.as_object().expect("body must be a JSON object");
        assert_eq!(
            obj.keys().collect::<Vec<_>>(),
            vec!["status"],
            "body must carry exactly one field"
        );
    }
}
