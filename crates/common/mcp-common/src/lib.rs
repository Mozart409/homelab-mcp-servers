//! Shared MCP server plumbing for the homelab-mcp-servers monorepo.
//!
//! Provides cross-server conventions that would otherwise be copy-pasted into
//! every crate, starting with the healthcheck probe and the `/_healthcheck` HTTP
//! route that the distroless containers rely on (there is no shell or curl).
//!
//! Also provides doc resource helpers for exposing crate README files as MCP resources.

use std::time::Duration;

use axum::{Json, Router, routing::get};
use color_eyre::eyre::{Context, Result, bail};
use rmcp::model::{ReadResourceResult, Resource, ResourceContents};
use serde::Serialize;

// ---- TLS --------------------------------------------------------------------

/// Installs `ring` as the process-wide rustls [`CryptoProvider`].
///
/// **Every code path that builds a `reqwest::Client` must call this first.**
///
/// The workspace asks reqwest for `rustls-no-provider` rather than `rustls`.
/// The two features are identical except that `rustls` also selects the
/// aws-lc-rs crypto provider — whose `aws-lc-sys` C build was, by a factor of
/// two, the most expensive crate in the workspace and forced `cmake` into the
/// container image. `rustls-no-provider` selects none, which means rustls has
/// no default to fall back on and `Client::builder().build()` fails at runtime
/// with `ClientCreationFailed` until a provider is installed here.
///
/// This changes the cipher suites and key exchanges on the wire; it does **not**
/// change which certificates are trusted. Root selection belongs to
/// `rustls-platform-verifier`, which both features enable, so the system trust
/// store — and the homelab CA installed on the deployment host — behaves as
/// before. The one thing ring cannot verify that aws-lc-rs can is an ECDSA
/// P-521 certificate.
///
/// Idempotent by construction: rustls' `install_default` reports an error when
/// a provider is already installed, which is the expected outcome for every
/// call after the first (several servers build more than one client, and each
/// test builds its own). That error is deliberately discarded.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ---- Doc resource helpers ---------------------------------------------------

/// Constructs the conventional URI for a server's operator guide.
///
/// Each MCP server exposes its crate README as a resource at a URI of the form
/// `doc://servername/guide` (e.g., `doc://hamcp/guide`), using this scheme to
/// distinguish documentation resources from file-system or web resources.
#[must_use]
pub fn doc_resource_uri(server: &str) -> String {
    format!("doc://{server}/guide")
}

/// Builds the MCP resource descriptor for a server's documentation.
///
/// Constructs a [`Resource`] that advertises a server's documentation (typically
/// from `include_str!("../README.md")`) to MCP clients. The resource includes
/// the provided URI, name, and optional description, and marks the MIME type
/// as `"text/markdown"` for proper client rendering.
///
/// Callers typically use [`doc_resource_uri`] to construct the URI, and pass
/// the server's name (e.g., `"hamcp"`) as both `name` and part of the URI.
#[must_use]
pub fn doc_resource(uri: &str, name: &str, description: &str) -> Resource {
    Resource::new(uri, name)
        .with_description(description)
        .with_mime_type("text/markdown")
}

/// Builds the result of reading a server's documentation resource.
///
/// Constructs a [`ReadResourceResult`] that carries the markdown content
/// of a server's documentation, suitable for immediate return from a
/// [`ServerHandler::read_resource`](https://docs.rs/rmcp/latest/rmcp/server/trait.ServerHandler.html#tymethod.read_resource)
/// handler.
///
/// The result wraps the markdown in a text resource with MIME type
/// `"text/markdown"`, matching the descriptor created by [`doc_resource`].
#[must_use]
pub fn doc_resource_contents(uri: &str, markdown: &str) -> ReadResourceResult {
    let contents = vec![ResourceContents::text(markdown, uri).with_mime_type("text/markdown")];
    ReadResourceResult::new(contents)
}

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

    install_crypto_provider();

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

    // ---- Doc resource tests -------------------------------------------------

    #[test]
    fn test_doc_resource_uri_format() {
        let uri = doc_resource_uri("hamcp");
        assert_eq!(uri, "doc://hamcp/guide");

        let uri = doc_resource_uri("pbsmcp");
        assert_eq!(uri, "doc://pbsmcp/guide");
    }

    #[test]
    fn test_doc_resource_descriptor_carries_uri_and_name() {
        let uri = "doc://hamcp/guide";
        let name = "hamcp-readme";
        let description = "HAMCP operator guide";

        let resource = doc_resource(uri, name, description);

        assert_eq!(resource.uri, uri);
        assert_eq!(resource.name, name);
        assert_eq!(
            resource.description,
            Some(description.to_string()),
            "description must be set"
        );
    }

    #[test]
    fn test_doc_resource_descriptor_sets_markdown_mime_type() {
        let resource = doc_resource("doc://test/guide", "test", "test doc");

        assert_eq!(
            resource.mime_type,
            Some("text/markdown".to_string()),
            "MIME type must be text/markdown"
        );
    }

    #[test]
    fn test_doc_resource_contents_carries_markdown() {
        let uri = "doc://hamcp/guide";
        let markdown = "# HAMCP\n\nThis is the operator guide.";

        let result = doc_resource_contents(uri, markdown);

        // ReadResourceResult wraps contents in a Vec
        assert_eq!(
            result.contents.len(),
            1,
            "must have exactly one content block"
        );

        // Extract the text content from the first (and only) ResourceContents enum variant
        let text_content = match result.contents.first() {
            Some(ResourceContents::TextResourceContents {
                uri: u,
                text: t,
                mime_type,
                ..
            }) => {
                assert_eq!(u, uri, "URI must match");
                assert_eq!(
                    mime_type,
                    &Some("text/markdown".to_string()),
                    "MIME type must be text/markdown"
                );
                t.as_str()
            }
            Some(ResourceContents::BlobResourceContents { .. }) => {
                panic!("expected TextResourceContents, got BlobResourceContents")
            }
            Some(_) => {
                panic!("unexpected ResourceContents variant")
            }
            None => {
                panic!("contents should not be empty")
            }
        };

        assert_eq!(text_content, markdown, "markdown content must round-trip");
    }

    #[test]
    fn test_doc_resource_descriptor_and_contents_round_trip() {
        let uri = "doc://custom/guide";
        let name = "custom-server";
        let description = "Custom server documentation";
        let markdown = "# Custom Server\n\nDocumentation here.";

        // Create the descriptor
        let descriptor = doc_resource(uri, name, description);

        // Create the contents
        let contents = doc_resource_contents(uri, markdown);

        // Verify the descriptor URI matches the contents URI
        assert_eq!(
            descriptor.uri, uri,
            "descriptor URI must match contents URI"
        );

        // Verify the contents URI from the resource content itself
        match contents.contents.first() {
            Some(ResourceContents::TextResourceContents {
                uri: content_uri, ..
            }) => {
                assert_eq!(content_uri, uri, "content URI must match");
            }
            Some(ResourceContents::BlobResourceContents { .. }) => {
                panic!("expected text content, got blob")
            }
            Some(_) => {
                panic!("unexpected ResourceContents variant")
            }
            None => {
                panic!("contents should not be empty")
            }
        }
    }
}
