//! Shared MCP server plumbing for the homelab-mcp-servers monorepo.
//!
//! Provides cross-server conventions that would otherwise be copy-pasted into
//! every crate, starting with the healthcheck probe and the `/_healthcheck` HTTP
//! route that the distroless containers rely on (there is no shell or curl).
//!
//! Also provides doc resource helpers for exposing crate README files as MCP resources.

#[cfg(feature = "e2e")]
pub mod e2e;

use std::{sync::Arc, time::Duration};

use axum::{Json, Router, routing::get};
use color_eyre::eyre::{Context, Result, bail};
use rmcp::{
    ServerHandler,
    model::{ReadResourceResult, Resource, ResourceContents},
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
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

// ---- Config helpers ---------------------------------------------------------

/// Turn a user-supplied host into a full base URL, defaulting the scheme to
/// `http` and the port to `default_port` when they are not already present.
///
/// Accepts the three spellings an operator actually types: a full URL
/// (`https://target.lan:9090`), a host with a port (`target.lan:9090`), and a
/// bare host (`target.lan`). Only the last gets `default_port` appended.
///
/// **The scheme default is `http`.** That suits the servers whose targets are
/// plain-HTTP internal services (Prometheus, Loki, Alertmanager). `pbsmcp` and
/// `wpmcp` default to `https` instead and deliberately do **not** use this
/// helper — folding their scheme rule in here would mean a second parameter
/// that exists solely to say "no, the other one".
#[must_use]
pub fn normalize_base_url(host: &str, default_port: u16) -> String {
    let h = host.trim().trim_end_matches('/');
    if h.starts_with("http://") || h.starts_with("https://") {
        h.to_string()
    } else if h.contains(':') {
        format!("http://{h}")
    } else {
        format!("http://{h}:{default_port}")
    }
}

/// Parse a comma-separated `Host` allow-list, treating "set but empty" as unset.
///
/// Returning `Some(vec![])` here would be actively dangerous: callers pass the
/// result to rmcp's `with_allowed_hosts`, and rmcp treats an empty allow-list as
/// "check disabled" — it accepts *every* inbound `Host` header, which is exactly
/// the DNS-rebinding hole the list exists to close. A value like `" , "` — a
/// typo, or a template that expanded to nothing — would therefore silently turn
/// the protection off. Collapsing that to `None` falls back to rmcp's
/// loopback-only default instead, which is the safe reading of "unset".
/// ([`mcp_router`] enforces the same rule a second time, for callers that build
/// a `Config` without going through this function.)
#[must_use]
pub fn parse_allowed_hosts(raw: Option<&str>) -> Option<Vec<String>> {
    let hosts: Vec<String> = raw?
        .split(',')
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .collect();

    if hosts.is_empty() { None } else { Some(hosts) }
}

// ---- URL path segments ------------------------------------------------------

/// Everything except RFC 3986 *unreserved* characters (`A-Z a-z 0-9 - . _ ~`).
///
/// Unreserved characters are left alone because RFC 3986 §2.3 says they
/// SHOULD NOT be encoded. `sensor.living_room` must reach Home Assistant as
/// itself, not as `sensor%2Eliving%5Froom`, which a strict router or a
/// path-matching proxy need not treat as the same resource.
const PATH_SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// A caller-supplied value that cannot be used as one URL path segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidPathSegment(String);

impl std::fmt::Display for InvalidPathSegment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} is not a valid path segment: empty, `.` and `..` would address a different \
             endpoint than the one this tool calls",
            self.0
        )
    }
}

impl std::error::Error for InvalidPathSegment {}

/// Percent-encode `s` as exactly one URL path segment, or refuse it.
///
/// Every server interpolates caller-supplied IDs (entity IDs, UPIDs, silence
/// IDs, label names) into request paths, and the MCP caller is an LLM acting
/// on untrusted input. The failure modes, and what guards each:
///
/// - **Structural characters split or end the path** (`/`, `?`, `#`, `%`): all
///   encoded.
/// - **A dot-segment climbs out of its path.** `url` (and so `reqwest`)
///   normalises `..` *and its percent-encoded forms* (`%2E%2E`, `.%2e`) per the
///   WHATWG URL spec, so `/api/states/%2E%2E` is sent as `/api/`. No encoding
///   can prevent that. Encoding `.` does not help, which is how this helper's
///   per-crate predecessors, encoding with `NON_ALPHANUMERIC`, let
///   `set_state("..")` POST to Home Assistant's API root. The only fix is to
///   **refuse** `.` and `..`.
/// - **An empty segment** turns `/api/states/{id}` into `/api/states/`, a
///   different endpoint (the collection). Refused too.
///
/// # Errors
///
/// Returns [`InvalidPathSegment`] for an empty string, `.` or `..`.
pub fn path_segment(s: &str) -> std::result::Result<String, InvalidPathSegment> {
    if s.is_empty() || s == "." || s == ".." {
        return Err(InvalidPathSegment(s.to_string()));
    }
    Ok(percent_encoding::utf8_percent_encode(s, PATH_SEGMENT).to_string())
}

// ---- Error rendering --------------------------------------------------------

/// `err` and every `source()` beneath it, joined with `": "`, skipping a level
/// that only repeats the one above.
///
/// The *cause* of a failed upstream call lives at the bottom of the chain:
/// reqwest's top level says only "error sending request for url (…)", while
/// "connection refused" or "invalid peer certificate: `UnknownIssuer`" is two
/// sources down. `Display` on a `thiserror` enum prints the top level alone,
/// so an MCP client (and the operator reading its answer) is told that
/// something failed but not what. eyre's `{:#}` already renders the chain;
/// this is the same for plain `std::error::Error` types.
#[must_use]
pub fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut parts: Vec<String> = vec![err.to_string()];
    let mut source = err.source();
    while let Some(cause) = source {
        let msg = cause.to_string();
        if parts.last().is_none_or(|last| !last.contains(&msg)) {
            parts.push(msg);
        }
        source = cause.source();
    }
    parts.join(": ")
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

// ---- Serving ----------------------------------------------------------------

/// Build the complete HTTP app a server exposes: the MCP service at `/mcp`
/// (streamable HTTP, one `S` per session from `factory`) plus [`health_router`].
///
/// This is the single place the app is assembled, so production (`run()` →
/// [`serve`]) and the end-to-end tests (`e2e::TestServer::start`) put the
/// *same* router on the socket — the DNS-rebinding allow-list included.
/// Tests that built their own router would exercise something that never runs
/// in production.
///
/// `allowed_hosts` of `None` — or `Some` of an empty list, see below — keeps
/// rmcp's loopback-only default; pass the config's value straight through.
pub fn mcp_router<S, F>(factory: F, allowed_hosts: Option<Vec<String>>) -> Router
where
    S: ServerHandler + Send + 'static,
    F: Fn() -> std::result::Result<S, std::io::Error> + Send + Sync + 'static,
{
    let mut http_config = StreamableHttpServerConfig::default();
    // `Some(empty)` must NOT reach rmcp: it reads an empty list as "accept any
    // Host", i.e. DNS-rebinding protection off. Treat it as unset (loopback
    // only) — failing closed, the same rule `parse_allowed_hosts` applies.
    if let Some(hosts) = allowed_hosts.filter(|h| !h.is_empty()) {
        http_config = http_config.with_allowed_hosts(hosts);
    }

    let service = StreamableHttpService::new(
        factory,
        Arc::new(LocalSessionManager::default()),
        http_config,
    );

    Router::new()
        .nest_service("/mcp", service)
        .merge(health_router())
}

/// Bind `bind` and serve `app` until the process is stopped.
///
/// `name` only labels the log line, so an operator reading a multi-server
/// journal can tell which listener came up where.
///
/// # Errors
///
/// Returns an error if the address cannot be bound (in use, bad syntax, no
/// permission) or the server loop fails.
pub async fn serve(bind: &str, app: Router, name: &str) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .wrap_err_with(|| format!("failed to bind {bind}"))?;
    tracing::info!(bind = %bind, "{name} listening on http://{bind}/mcp");

    axum::serve(listener, app)
        .await
        .wrap_err("streamable-HTTP server error")
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

        // `reqwest::get` builds a Client of its own, which panics under
        // `rustls-no-provider` unless a provider is already installed. Unlike
        // the probe tests this path never goes through `run_healthcheck`, so
        // whether it worked depended on another test happening to install one
        // first — install it here instead of relying on test ordering.
        install_crypto_provider();

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
    fn normalize_base_url_appends_the_default_port_to_a_bare_host() {
        assert_eq!(
            normalize_base_url("target.lan", 9090),
            "http://target.lan:9090"
        );
    }

    #[test]
    fn normalize_base_url_keeps_an_explicit_port() {
        assert_eq!(
            normalize_base_url("target.lan:3100", 9090),
            "http://target.lan:3100"
        );
    }

    #[test]
    fn normalize_base_url_preserves_an_explicit_scheme_and_trims_trailing_slash() {
        assert_eq!(
            normalize_base_url("https://target.example.com/", 9090),
            "https://target.example.com"
        );
        assert_eq!(
            normalize_base_url("  http://target.lan:9093  ", 9090),
            "http://target.lan:9093"
        );
    }

    #[test]
    fn parse_allowed_hosts_treats_unset_and_blank_as_none() {
        // `Some(vec![])` would reject every inbound Host header; see the doc
        // comment on `parse_allowed_hosts`.
        assert!(parse_allowed_hosts(None).is_none());
        assert!(parse_allowed_hosts(Some("")).is_none());
        assert!(parse_allowed_hosts(Some(" , ")).is_none());
    }

    #[test]
    fn parse_allowed_hosts_splits_and_trims() {
        assert_eq!(
            parse_allowed_hosts(Some("a.lan, b.lan ,c.lan")),
            Some(vec![
                "a.lan".to_string(),
                "b.lan".to_string(),
                "c.lan".to_string()
            ])
        );
    }
}
