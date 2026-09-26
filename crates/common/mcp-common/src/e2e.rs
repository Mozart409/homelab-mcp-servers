//! End-to-end test harness: a real server on a real socket, driven over the
//! real MCP wire protocol.
//!
//! Enabled by the `e2e` feature, which server crates turn on from
//! `[dev-dependencies]` only. The shape of every E2E test in this workspace is:
//!
//! 1. stand up the upstream: a `wiremock::MockServer` with realistic payloads
//!    for REST servers, a throwaway database for pgmcp;
//! 2. build the server's production router ([`crate::mcp_router`], via the
//!    crate's `router(&Config)`) and bind it with [`TestServer::start`];
//! 3. talk to it with [`McpClient`]: `initialize` handshake, `tools/list`,
//!    `tools/call`, exactly as Claude or any other MCP client would;
//! 4. snapshot the **exchange** with a [`Scenario`]: each tool call, the
//!    requests it caused upstream ([`upstream_requests`]), and the MCP response
//!    ([`ToolResult::transcript`]). The committed `.snap` files are the test's
//!    verifiable, repeatable artifact. A reviewer can read what the server
//!    sends and returns without running anything, and any behaviour change
//!    shows up as a diff.
//!
//! # Why a hand-rolled JSON-RPC client and not rmcp's
//!
//! The point is to test the server as a client sees it. rmcp's own client
//! shares types, serde settings and protocol assumptions with the server, so a
//! bug in how rmcp encodes something would be decoded symmetrically and
//! stay hidden. Speaking raw JSON-RPC over `reqwest` means the only thing both
//! sides share is the wire. It also lets a test send what no well-behaved
//! client would, such as a foreign `Host` header for the DNS-rebinding guard.
//!
//! # How an E2E test can lie, and what guards each case
//!
//! - **Testing a router production never runs.** [`TestServer::start`] takes
//!   the crate's `router(&Config)`, which is the one `run()` serves.
//! - **The handshake is skipped, so session and protocol-version bugs hide.**
//!   [`McpClient::connect`] always does `initialize` → `notifications/initialized`
//!   and sends `Mcp-Session-Id` and `MCP-Protocol-Version` on every request after.
//! - **Reading the wrong SSE event.** Responses arrive as `text/event-stream`
//!   with priming and keep-alive events. [`McpClient::request`] matches on the
//!   JSON-RPC `id`, not on "the first `data:` line".
//! - **A tool error mistaken for success.** A failed tool is either a JSON-RPC
//!   `error` or a result with `isError: true`. [`ToolResult::text`] refuses both,
//!   so `.text()?` in a happy-path test cannot pass on an error body.
//! - **An upstream mock that nobody called.** Snapshotting [`upstream_requests`]
//!   records exactly which requests happened. A tool that answered from nowhere
//!   (or called twice) changes the artifact.
//! - **A snapshot that churns on every run.** Ephemeral ports are scrubbed
//!   ([`scrub`], applied by [`Scenario`]), and tests redact the crate version
//!   out of `serverInfo`, so a diff means behaviour changed.
//! - **A hang instead of a failure.** Every HTTP call has a timeout.

use std::{
    net::SocketAddr,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use axum::Router;
use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use reqwest::{StatusCode, header::HeaderMap};
use serde_json::{Map, Value, json};
use tokio::{net::TcpListener, task::JoinHandle};

/// The protocol version the harness offers in `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Upper bound on any single HTTP exchange with the server under test.
///
/// Generous, because pgmcp's statement-timeout test deliberately waits on the
/// database. A tool that hangs past this fails the test instead of wedging CI.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// ---- Server -----------------------------------------------------------------

/// A server under test, listening on an ephemeral loopback port.
///
/// Dropping it aborts the serve task, so each test owns its server outright
/// and parallel tests never share state through one.
pub struct TestServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl TestServer {
    /// Bind `127.0.0.1:0` and serve `app` on it in the background.
    ///
    /// # Errors
    ///
    /// Returns an error if no loopback port can be bound.
    pub async fn start(app: Router) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .wrap_err("failed to bind an ephemeral port for the server under test")?;
        let addr = listener
            .local_addr()
            .wrap_err("failed to read the server's ephemeral port")?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { addr, task })
    }

    /// The bound address, e.g. `127.0.0.1:41234`.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The streamable-HTTP endpoint, `http://<addr>/mcp`.
    #[must_use]
    pub fn mcp_url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    /// Open an initialized MCP session against this server.
    ///
    /// # Errors
    ///
    /// See [`McpClient::connect`].
    pub async fn connect(&self) -> Result<McpClient> {
        McpClient::connect(&self.mcp_url()).await
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// ---- Client -----------------------------------------------------------------

/// A raw HTTP response from the MCP endpoint, for tests that assert on the
/// transport itself (status codes, rejected hosts) rather than on JSON-RPC.
#[derive(Debug)]
pub struct RawResponse {
    /// HTTP status.
    pub status: StatusCode,
    /// Response headers.
    pub headers: HeaderMap,
    /// Response body, undecoded.
    pub body: String,
}

/// An initialized MCP session over streamable HTTP.
pub struct McpClient {
    http: reqwest::Client,
    url: String,
    session_id: Option<String>,
    protocol_version: String,
    initialize_result: Value,
    next_id: AtomicU64,
}

impl McpClient {
    /// Connect to `url` and complete the MCP handshake: `initialize`, then the
    /// `notifications/initialized` notification.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be built, the server rejects
    /// `initialize` (non-2xx, JSON-RPC error, no result), or it does not accept
    /// the `initialized` notification with `202 Accepted`.
    pub async fn connect(url: &str) -> Result<Self> {
        crate::install_crypto_provider();
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .wrap_err("failed to build the e2e HTTP client")?;

        let mut client = Self {
            http,
            url: url.to_string(),
            session_id: None,
            protocol_version: PROTOCOL_VERSION.to_string(),
            initialize_result: Value::Null,
            next_id: AtomicU64::new(1),
        };

        let body = initialize_body(0);
        let raw = client.post(&body, None).await?;
        if !raw.status.is_success() {
            bail!("initialize returned {}: {}", raw.status, raw.body);
        }
        client.session_id = raw
            .headers
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let message = extract_message(&raw, 0)?;
        let result = message
            .get("result")
            .cloned()
            .ok_or_else(|| eyre!("initialize returned no result: {message}"))?;
        if let Some(version) = result.get("protocolVersion").and_then(Value::as_str) {
            client.protocol_version = version.to_string();
        }
        client.initialize_result = result;

        let notified = client
            .post(
                &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                None,
            )
            .await?;
        if notified.status != StatusCode::ACCEPTED {
            bail!(
                "notifications/initialized returned {} (expected 202): {}",
                notified.status,
                notified.body
            );
        }

        Ok(client)
    }

    /// The `initialize` result: `serverInfo`, `capabilities`, `instructions`.
    #[must_use]
    pub fn initialize_result(&self) -> &Value {
        &self.initialize_result
    }

    /// Send one JSON-RPC request and return the full response message (the
    /// object carrying `result` or `error`).
    ///
    /// # Errors
    ///
    /// Returns an error on a transport failure, a non-2xx status, or a body
    /// that holds no JSON-RPC message with this request's `id`.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let raw = self.post(&body, None).await?;
        if !raw.status.is_success() {
            bail!("{method} returned HTTP {}: {}", raw.status, raw.body);
        }
        extract_message(&raw, id)
    }

    /// `tools/list`, following `nextCursor` until the server stops paginating.
    ///
    /// # Errors
    ///
    /// Returns an error if any page is a JSON-RPC error or lacks `tools`.
    pub async fn list_tools(&self) -> Result<Vec<Value>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |c| json!({ "cursor": c }));
            let result = into_result(self.request("tools/list", params).await?)?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| eyre!("tools/list result has no `tools` array: {result}"))?;
            tools.extend(page.iter().cloned());
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
                _ => return Ok(tools),
            }
        }
    }

    /// The names from [`list_tools`](Self::list_tools), sorted.
    ///
    /// # Errors
    ///
    /// As [`list_tools`](Self::list_tools).
    pub async fn tool_names(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = self
            .list_tools()
            .await?
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string))
            .collect();
        names.sort();
        Ok(names)
    }

    /// `tools/call` with `arguments`. A tool-level failure is **not** an `Err`
    /// here, because asserting on failures is half of what the tests do. It
    /// comes back inside [`ToolResult`].
    ///
    /// # Errors
    ///
    /// Returns an error only on a transport or framing failure.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResult> {
        let message = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        Ok(ToolResult { message })
    }

    /// `resources/list` → the `resources` array.
    ///
    /// # Errors
    ///
    /// Returns an error on a JSON-RPC error or a result without `resources`.
    pub async fn list_resources(&self) -> Result<Vec<Value>> {
        let result = into_result(self.request("resources/list", json!({})).await?)?;
        result
            .get("resources")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| eyre!("resources/list result has no `resources` array: {result}"))
    }

    /// `resources/read` → the `contents` array.
    ///
    /// # Errors
    ///
    /// Returns an error on a JSON-RPC error or a result without `contents`.
    pub async fn read_resource(&self, uri: &str) -> Result<Vec<Value>> {
        let result = into_result(
            self.request("resources/read", json!({ "uri": uri }))
                .await?,
        )?;
        result
            .get("contents")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| eyre!("resources/read result has no `contents` array: {result}"))
    }

    /// `prompts/list` → the `prompts` array.
    ///
    /// # Errors
    ///
    /// Returns an error on a JSON-RPC error or a result without `prompts`.
    pub async fn list_prompts(&self) -> Result<Vec<Value>> {
        let result = into_result(self.request("prompts/list", json!({})).await?)?;
        result
            .get("prompts")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| eyre!("prompts/list result has no `prompts` array: {result}"))
    }

    /// `prompts/get` → the full result (`description`, `messages`).
    ///
    /// # Errors
    ///
    /// Returns an error on a JSON-RPC error.
    pub async fn get_prompt(&self, name: &str, arguments: Value) -> Result<Value> {
        into_result(
            self.request(
                "prompts/get",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?,
        )
    }

    /// POST a JSON body to the endpoint with the session headers, optionally
    /// overriding `Host`, and return the undecoded response.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent or the body not read.
    pub async fn post(&self, body: &Value, host: Option<&str>) -> Result<RawResponse> {
        let mut req = self
            .http
            .post(&self.url)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .header("mcp-protocol-version", &self.protocol_version)
            .json(body);
        if let Some(sid) = &self.session_id {
            req = req.header("mcp-session-id", sid);
        }
        if let Some(host) = host {
            req = req.header("host", host);
        }
        let resp = req
            .send()
            .await
            .wrap_err_with(|| format!("POST {} failed", self.url))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp
            .text()
            .await
            .wrap_err_with(|| format!("reading the body of POST {} failed", self.url))?;
        Ok(RawResponse {
            status,
            headers,
            body,
        })
    }
}

/// Send a bare `initialize` to `url` with the given `Host` header, outside any
/// session. This is the request a DNS-rebinding attack would make, so it is
/// what the allow-list tests use.
///
/// # Errors
///
/// Returns an error only if the request cannot be sent at all. Any HTTP status
/// is a successful return, since the status is what the test inspects.
pub async fn initialize_with_host(url: &str, host: &str) -> Result<RawResponse> {
    crate::install_crypto_provider();
    let http = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .wrap_err("failed to build the e2e HTTP client")?;
    let resp = http
        .post(url)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("host", host)
        .json(&initialize_body(0))
        .send()
        .await
        .wrap_err_with(|| format!("POST {url} failed"))?;
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp.text().await.unwrap_or_default();
    Ok(RawResponse {
        status,
        headers,
        body,
    })
}

fn initialize_body(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "mcp-common-e2e", "version": "0" }
        }
    })
}

/// Pull the JSON-RPC message with `id` out of a response that is either plain
/// JSON or an SSE stream.
fn extract_message(raw: &RawResponse, id: u64) -> Result<Value> {
    let is_sse = raw
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"));

    let candidates: Vec<Value> = if is_sse {
        sse_events(&raw.body)
            .iter()
            .filter_map(|data| serde_json::from_str(data).ok())
            .collect()
    } else {
        vec![
            serde_json::from_str(&raw.body)
                .wrap_err_with(|| format!("response is not JSON: {}", raw.body))?,
        ]
    };

    candidates
        .into_iter()
        .find(|m| m.get("id").and_then(Value::as_u64) == Some(id))
        .ok_or_else(|| eyre!("no JSON-RPC message with id {id} in response: {}", raw.body))
}

/// The `data` payload of each SSE event in `body`, multi-line data joined with
/// `\n` as the SSE spec requires. Events with no data (priming, keep-alive)
/// are dropped.
fn sse_events(body: &str) -> Vec<String> {
    let mut events = Vec::new();
    let mut data: Vec<&str> = Vec::new();
    for line in body.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !data.is_empty() {
                events.push(data.join("\n"));
                data.clear();
            }
        } else if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    events
}

/// `message.result`, or an error carrying `message.error`.
fn into_result(mut message: Value) -> Result<Value> {
    if let Some(err) = message.get("error") {
        bail!("JSON-RPC error: {err}");
    }
    message
        .get_mut("result")
        .map(Value::take)
        .ok_or_else(|| eyre!("JSON-RPC message has neither result nor error: {message}"))
}

// ---- Tool results -----------------------------------------------------------

/// The outcome of one `tools/call`: success, a tool error (`isError: true`), or
/// a JSON-RPC protocol error. All three are data, so the test decides which it
/// expected.
#[derive(Debug, Clone)]
pub struct ToolResult {
    message: Value,
}

impl ToolResult {
    /// The raw JSON-RPC response message.
    #[must_use]
    pub fn message(&self) -> &Value {
        &self.message
    }

    /// `true` only for a `result` without `isError: true`.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.message
            .get("result")
            .is_some_and(|r| !r.get("isError").and_then(Value::as_bool).unwrap_or(false))
    }

    /// The concatenated `text` content of a **successful** call.
    ///
    /// # Errors
    ///
    /// Returns an error if the call failed in either way, so a happy-path
    /// assertion can never pass on an error body.
    pub fn text(&self) -> Result<String> {
        if !self.is_success() {
            bail!("tool call did not succeed: {}", self.message);
        }
        Ok(self.texts().join(""))
    }

    /// [`text`](Self::text), parsed as JSON.
    ///
    /// # Errors
    ///
    /// As [`text`](Self::text), or if the text is not JSON.
    pub fn json(&self) -> Result<Value> {
        let text = self.text()?;
        serde_json::from_str(&text).wrap_err_with(|| format!("tool text is not JSON: {text}"))
    }

    /// The failure description of a call that did **not** succeed: the
    /// JSON-RPC error message, or the text of an `isError` result.
    ///
    /// # Errors
    ///
    /// Returns an error if the call succeeded, so an error-path assertion can
    /// never pass on a success.
    pub fn error_message(&self) -> Result<String> {
        if self.is_success() {
            bail!(
                "expected the tool call to fail, but it succeeded: {}",
                self.message
            );
        }
        if let Some(msg) = self
            .message
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
        {
            return Ok(msg.to_string());
        }
        Ok(self.texts().join(""))
    }

    /// A stable, readable rendering for snapshots: either
    /// `{"ok": <content>}` or `{"error": {"code", "message"}}` /
    /// `{"tool_error": <content>}`. Text that parses as JSON is embedded as
    /// JSON, not as an escaped string, so the snapshot diffs by field.
    #[must_use]
    pub fn transcript(&self) -> Value {
        if let Some(err) = self.message.get("error") {
            let mut out = Map::new();
            if let Some(code) = err.get("code") {
                out.insert("code".into(), code.clone());
            }
            if let Some(msg) = err.get("message") {
                out.insert("message".into(), msg.clone());
            }
            return json!({ "error": out });
        }
        let content: Vec<Value> = self
            .texts()
            .into_iter()
            .map(|t| serde_json::from_str(&t).unwrap_or(Value::String(t)))
            .collect();
        let content = match <[Value; 1]>::try_from(content) {
            Ok([single]) => single,
            Err(many) => Value::Array(many),
        };
        if self.is_success() {
            json!({ "ok": content })
        } else {
            json!({ "tool_error": content })
        }
    }

    fn texts(&self) -> Vec<String> {
        self.message
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|c| c.get("text").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ---- Upstream recording -----------------------------------------------------

/// Every request `mock` received, in arrival order, as snapshot-ready JSON:
/// `method`, `path`, `query` (ordered pairs, so repeated keys stay visible),
/// `authorization` and `x-scope-orgid` when sent, and `body` (as JSON when it
/// parses).
///
/// Other headers (user agent, host with its ephemeral port) are left out on
/// purpose: they are noise in a diff and say nothing about the server.
///
/// # Errors
///
/// Returns an error if the mock was built with request recording disabled.
pub async fn upstream_requests(mock: &wiremock::MockServer) -> Result<Value> {
    let requests = mock
        .received_requests()
        .await
        .ok_or_else(|| eyre!("wiremock request recording is disabled"))?;

    let rendered = requests
        .iter()
        .map(|r| {
            let mut out = Map::new();
            out.insert("method".into(), Value::String(r.method.to_string()));
            out.insert("path".into(), Value::String(r.url.path().to_string()));
            let query: Vec<Value> = r.url.query_pairs().map(|(k, v)| json!([k, v])).collect();
            if !query.is_empty() {
                out.insert("query".into(), Value::Array(query));
            }
            for name in ["authorization", "x-scope-orgid"] {
                if let Some(v) = r.headers.get(name).and_then(|v| v.to_str().ok()) {
                    out.insert(name.into(), Value::String(v.to_string()));
                }
            }
            if !r.body.is_empty() {
                let body = serde_json::from_slice(&r.body).unwrap_or_else(|_| {
                    Value::String(String::from_utf8_lossy(&r.body).into_owned())
                });
                out.insert("body".into(), body);
            }
            Value::Object(out)
        })
        .collect();
    Ok(Value::Array(rendered))
}

/// The HTTP methods `mock` received, deduplicated and sorted, as a quick
/// read-only guard (`== ["GET"]`).
///
/// # Errors
///
/// As [`upstream_requests`].
pub async fn upstream_methods(mock: &wiremock::MockServer) -> Result<Vec<String>> {
    let requests = mock
        .received_requests()
        .await
        .ok_or_else(|| eyre!("wiremock request recording is disabled"))?;
    let mut methods: Vec<String> = requests.iter().map(|r| r.method.to_string()).collect();
    methods.sort();
    methods.dedup();
    Ok(methods)
}

/// A recorded conversation with the server: each [`call`](Self::call) appends
/// one step holding what was asked, **only the upstream requests that call
/// caused**, and what came back. [`transcript`](Self::transcript) is the
/// snapshot artifact.
///
/// Recording per step, rather than dumping the mock's whole log at the end,
/// is what makes a multi-call scenario reviewable. The snapshot shows that
/// `task_log` with `tail` made two upstream requests, a probe and then the
/// window, and that the call before it made one.
///
/// The mock's URI is scrubbed to `http://upstream` in every step, so ephemeral
/// ports never reach a `.snap` file.
pub struct Scenario<'a> {
    client: &'a McpClient,
    mock: &'a wiremock::MockServer,
    seen: usize,
    steps: Vec<Value>,
}

impl<'a> Scenario<'a> {
    /// Start recording calls made through `client` against `mock`.
    #[must_use]
    pub fn new(client: &'a McpClient, mock: &'a wiremock::MockServer) -> Self {
        Self {
            client,
            mock,
            seen: 0,
            steps: Vec::new(),
        }
    }

    /// Call `tool` with `arguments`, record the step, and return the result so
    /// the test can also assert on it directly.
    ///
    /// # Errors
    ///
    /// Returns an error on a transport failure or if request recording is off.
    pub async fn call(&mut self, tool: &str, arguments: Value) -> Result<ToolResult> {
        let result = self.client.call_tool(tool, arguments.clone()).await?;
        let all = upstream_requests(self.mock).await?;
        let upstream: Vec<Value> = all
            .as_array()
            .map(|a| a.iter().skip(self.seen).cloned().collect())
            .unwrap_or_default();
        self.seen += upstream.len();
        let step = json!({
            "call": { "tool": tool, "arguments": arguments },
            "upstream": upstream,
            "response": result.transcript(),
        });
        self.steps
            .push(scrub(step, &self.mock.uri(), "http://upstream"));
        Ok(result)
    }

    /// Every recorded step, in order.
    #[must_use]
    pub fn transcript(&self) -> Value {
        Value::Array(self.steps.clone())
    }
}

/// Everything a client can learn about the server without calling a tool, as
/// one snapshot-ready value: the `initialize` result (with `serverInfo.version`
/// redacted, so a release bump does not churn every snapshot), `tools/list`
/// with full input schemas, `prompts/list`, `resources/list`, and **every
/// prompt rendered twice**, once with only its required arguments and once with
/// all of them (each set to `"<name>"`).
///
/// Driven from the lists themselves, so a new tool, prompt or argument shows up
/// in the snapshot diff the moment it is registered.
///
/// # Errors
///
/// Returns an error if any list or `prompts/get` call fails, including a
/// prompt that cannot render with just its required arguments.
pub async fn contract(client: &McpClient) -> Result<Value> {
    let mut initialize = client.initialize_result().clone();
    if let Some(v) = initialize.pointer_mut("/serverInfo/version") {
        *v = json!("[version]");
    }
    let prompts = client.list_prompts().await?;
    let mut rendered = Map::new();
    for prompt in &prompts {
        let Some(name) = prompt.get("name").and_then(Value::as_str) else {
            continue;
        };
        let args: Vec<&Value> = prompt
            .get("arguments")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        let fill = |only_required: bool| -> Value {
            let mut m = Map::new();
            for a in &args {
                let required = a.get("required").and_then(Value::as_bool).unwrap_or(false);
                if let Some(n) = a.get("name").and_then(Value::as_str)
                    && (required || !only_required)
                {
                    m.insert(n.to_string(), json!(format!("<{n}>")));
                }
            }
            Value::Object(m)
        };
        rendered.insert(
            format!("{name}/minimal"),
            client.get_prompt(name, fill(true)).await?,
        );
        rendered.insert(
            format!("{name}/full"),
            client.get_prompt(name, fill(false)).await?,
        );
    }
    Ok(json!({
        "initialize": initialize,
        "tools": client.list_tools().await?,
        "prompts": prompts,
        "resources": client.list_resources().await?,
        "rendered_prompts": rendered,
    }))
}

/// Read the doc resource at `uri` and require it to be `expected` verbatim
/// (the crate README, via `include_str!` in the test), as one markdown text
/// block under the URI that was asked for.
///
/// # Errors
///
/// Returns an error if the read fails, or the contents are not exactly one
/// `text/markdown` block for `uri` carrying `expected`.
pub async fn check_doc_resource(client: &McpClient, uri: &str, expected: &str) -> Result<()> {
    let contents = client.read_resource(uri).await?;
    let [block] = contents.as_slice() else {
        bail!("{uri} returned {} content blocks, want 1", contents.len());
    };
    let field = |k: &str| block.get(k).and_then(Value::as_str);
    if field("uri") != Some(uri) {
        bail!("{uri} answered for {:?}", field("uri"));
    }
    if field("mimeType") != Some("text/markdown") {
        bail!("{uri} has MIME type {:?}", field("mimeType"));
    }
    let text = field("text").ok_or_else(|| eyre!("{uri} has no text content"))?;
    if text != expected {
        bail!("{uri} does not serve the crate README verbatim");
    }
    Ok(())
}

/// The DNS-rebinding guard, three ways. `router` builds the server's
/// production router for a given `allowed_hosts` setting.
///
/// 1. Default (`None`): a foreign `Host` is refused with 403, and `localhost`
///    and `127.0.0.1` (with the port) are served.
/// 2. Configured (`Some([name])`): that name is served, a foreign one refused.
/// 3. **Empty (`Some([])`) fails closed.** rmcp reads an empty list as "accept
///    any Host", so this is the case that would silently turn the protection
///    off. [`crate::mcp_router`] must treat it as unset.
///
/// # Errors
///
/// Returns an error naming the first case that failed.
pub async fn check_host_allow_list<F>(router: F) -> Result<()>
where
    F: Fn(Option<Vec<String>>) -> Result<Router>,
{
    const EVIL: &str = "attacker.example";
    const NAMED: &str = "mcp.homelab.local";

    let server = TestServer::start(router(None)?).await?;
    let port = server.addr().port();
    let evil = initialize_with_host(&server.mcp_url(), EVIL).await?;
    if evil.status != StatusCode::FORBIDDEN {
        bail!("default allow-list served Host {EVIL}: {}", evil.status);
    }
    for ok in [format!("localhost:{port}"), format!("127.0.0.1:{port}")] {
        let res = initialize_with_host(&server.mcp_url(), &ok).await?;
        if !res.status.is_success() {
            bail!(
                "default allow-list refused loopback Host {ok}: {}",
                res.status
            );
        }
    }

    let server = TestServer::start(router(Some(vec![NAMED.to_string()]))?).await?;
    let named = initialize_with_host(&server.mcp_url(), NAMED).await?;
    if !named.status.is_success() {
        bail!("configured Host {NAMED} was refused: {}", named.status);
    }
    let evil = initialize_with_host(&server.mcp_url(), EVIL).await?;
    if evil.status != StatusCode::FORBIDDEN {
        bail!("configured allow-list served Host {EVIL}: {}", evil.status);
    }

    let server = TestServer::start(router(Some(vec![]))?).await?;
    let evil = initialize_with_host(&server.mcp_url(), EVIL).await?;
    if evil.status != StatusCode::FORBIDDEN {
        bail!(
            "an EMPTY allow-list served Host {EVIL} ({}): it must fail closed, not open",
            evil.status
        );
    }
    Ok(())
}

/// The smallest **valid** arguments object for `tool`'s `inputSchema`: every
/// `required` property gets a type-appropriate placeholder (the first `enum`
/// value, `"sample"`, `1`, `false`), optional ones are omitted. `$ref`s are
/// resolved against the schema's `$defs`; an array gets **one** element and an
/// object its own required fields, recursively.
///
/// Valid matters. An empty array for a required list (a silence's `matchers`)
/// is refused by the server before any request is made, which would make the
/// tool look like it sends nothing. One well-formed element makes the call
/// real.
///
/// This drives the tests that must cover **every** tool, above all the
/// read/write surface map, straight from `tools/list`, so a newly added tool is
/// covered the moment it is registered, with no test edit to forget.
#[must_use]
pub fn minimal_arguments(tool: &Value) -> Value {
    let schema = tool.get("inputSchema").cloned().unwrap_or_default();
    placeholder(&schema, &schema, 0)
}

/// Placeholder for `prop` in the schema rooted at `root`. `depth` bounds
/// recursion through self-referential `$ref`s.
fn placeholder(prop: &Value, root: &Value, depth: u8) -> Value {
    if depth > 8 {
        return Value::Null;
    }
    if let Some(target) = prop.get("$ref").and_then(Value::as_str) {
        // `#/$defs/Name` (or the older `#/definitions/Name`).
        let pointer = target.trim_start_matches('#');
        return root
            .pointer(pointer)
            .map_or(Value::Null, |def| placeholder(def, root, depth + 1));
    }
    for combinator in ["anyOf", "oneOf", "allOf"] {
        if let Some(first) = prop
            .get(combinator)
            .and_then(Value::as_array)
            .and_then(|variants| {
                variants
                    .iter()
                    .find(|v| v.get("type") != Some(&json!("null")))
            })
        {
            return placeholder(first, root, depth + 1);
        }
    }
    if let Some(first) = prop
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|e| e.first())
    {
        return first.clone();
    }
    // `type` is a string, or an array such as `["string", "null"]`.
    let types: Vec<&str> = match prop.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    let is_object = prop.get("properties").is_some();
    match types.into_iter().find(|t| *t != "null") {
        Some("integer" | "number") => json!(1),
        Some("boolean") => json!(false),
        Some("array") => {
            let item = prop
                .get("items")
                .map_or_else(|| json!("sample"), |i| placeholder(i, root, depth + 1));
            json!([item])
        }
        Some("object") => object_placeholder(prop, root, depth),
        _ if is_object => object_placeholder(prop, root, depth),
        _ => json!("sample"),
    }
}

fn object_placeholder(prop: &Value, root: &Value, depth: u8) -> Value {
    let props = prop.get("properties").cloned().unwrap_or_default();
    let mut out = Map::new();
    for name in prop
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let p = props.get(name).cloned().unwrap_or_default();
        out.insert(name.to_string(), placeholder(&p, root, depth + 1));
    }
    Value::Object(out)
}

/// Call every tool the server advertises, once each with
/// [`minimal_arguments`], against a catch-all upstream answering every request
/// with `200` and `body`, and return `{tool: [HTTP methods it sent]}`.
///
/// Snapshot the result: it is the server's read/write surface. Hard rule §1
/// says every tool issues only GETs, except the few named exceptions, and this
/// map is the proof. A new tool that sends a POST, or an existing one that
/// starts to, changes the snapshot and cannot land unnoticed. Tool-level
/// failures (the catch-all body not matching what a tool parses) are expected
/// and irrelevant, since the question is only what went over the wire.
///
/// # Errors
///
/// Returns an error if `tools/list` fails or a call fails at the transport.
pub async fn method_surface(
    client: &McpClient,
    mock: &wiremock::MockServer,
    body: &Value,
) -> Result<Value> {
    let mut surface = Map::new();
    for tool in client.list_tools().await? {
        let Some(name) = tool.get("name").and_then(Value::as_str) else {
            continue;
        };
        mock.reset().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
            .mount(mock)
            .await;
        client.call_tool(name, minimal_arguments(&tool)).await?;
        let methods = upstream_methods(mock).await?;
        surface.insert(name.to_string(), json!(methods));
    }
    mock.reset().await;
    Ok(Value::Object(surface))
}

/// Replace every occurrence of `needle` in every string (keys excluded) of
/// `value` with `replacement`. For ephemeral addresses and other per-run noise.
#[must_use]
pub fn scrub(value: Value, needle: &str, replacement: &str) -> Value {
    match value {
        Value::String(s) => Value::String(s.replace(needle, replacement)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| scrub(v, needle, replacement))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, scrub(v, needle, replacement)))
                .collect(),
        ),
        other => other,
    }
}

// ---- TLS upstream -----------------------------------------------------------

/// A TLS front for a plain-HTTP mock: terminates TLS with a freshly generated
/// **self-signed** certificate (for `127.0.0.1`) and pipes the bytes to
/// `target`. Dropping it stops accepting.
///
/// wiremock speaks only HTTP, so without this no test could reach the code
/// behind every server's `*_INSECURE` flag, and none did. That is how
/// `HA_INSECURE` shipped documented, parsed, and never applied.
pub struct TlsProxy {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl TlsProxy {
    /// Start the proxy in front of `target`.
    ///
    /// # Errors
    ///
    /// Returns an error if the certificate cannot be generated, the TLS
    /// config cannot be built, or no loopback port can be bound.
    pub async fn start(target: SocketAddr) -> Result<Self> {
        use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

        crate::install_crypto_provider();
        let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
            .wrap_err("failed to generate a self-signed certificate")?;
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            generated.signing_key.serialize_der(),
        ));
        let config = tokio_rustls::rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![generated.cert.der().clone()], key)
            .wrap_err("failed to build the TLS server config")?;
        let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config));

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .wrap_err("failed to bind the TLS proxy")?;
        let addr = listener
            .local_addr()
            .wrap_err("failed to read the TLS proxy port")?;
        let task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    // A client that refuses the certificate fails the
                    // handshake here; that is the expected outcome in the
                    // verify-on case, not an error of the proxy.
                    let Ok(mut tls) = acceptor.accept(socket).await else {
                        return;
                    };
                    if let Ok(mut upstream) = tokio::net::TcpStream::connect(target).await {
                        let _ = tokio::io::copy_bidirectional(&mut tls, &mut upstream).await;
                    }
                });
            }
        });
        Ok(Self { addr, task })
    }

    /// `https://127.0.0.1:<port>`, the base URL to configure a server with.
    #[must_use]
    pub fn url(&self) -> String {
        format!("https://{}", self.addr)
    }
}

impl Drop for TlsProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Prove a server's `insecure` setting both ways, against a self-signed
/// upstream in front of `mock` (which must answer `tool`'s request):
///
/// 1. `insecure = false`: the call **fails**, naming the certificate. The
///    failure mode is a client that skips verification by default.
/// 2. `insecure = true`: the same call **succeeds**. The failure mode is a
///    flag that is parsed but never applied to the HTTP client.
///
/// `router` builds the server's production router for a base URL and an
/// `insecure` value.
///
/// # Errors
///
/// Returns an error naming the half that failed.
pub async fn check_insecure_flag<F>(
    mock: &wiremock::MockServer,
    router: F,
    tool: &str,
    arguments: Value,
) -> Result<()>
where
    F: Fn(&str, bool) -> Result<Router>,
{
    let proxy = TlsProxy::start(*mock.address()).await?;

    let server = TestServer::start(router(&proxy.url(), false)?).await?;
    let verified = server
        .connect()
        .await?
        .call_tool(tool, arguments.clone())
        .await?;
    let Ok(err) = verified.error_message() else {
        bail!("{tool} succeeded against a self-signed upstream with verification on");
    };
    if !err.to_lowercase().contains("certificate") && !err.to_lowercase().contains("tls") {
        bail!("{tool} failed against a self-signed upstream, but not on TLS: {err}");
    }

    let server = TestServer::start(router(&proxy.url(), true)?).await?;
    let insecure = server.connect().await?.call_tool(tool, arguments).await?;
    if !insecure.is_success() {
        bail!(
            "{tool} failed against a self-signed upstream with `insecure` on, so the flag is not \
             applied: {}",
            insecure.message()
        );
    }
    Ok(())
}

// ---- Real binaries ----------------------------------------------------------

/// How long a spawned binary gets to answer `/_healthcheck` before the test
/// gives up on it.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

/// A free loopback address to hand a binary as its `*_BIND`.
///
/// There is a window between releasing the port here and the binary binding it.
/// Another process could take it in that window, which would surface as a
/// startup failure naming the address, never as a wrong result.
///
/// # Errors
///
/// Returns an error if no loopback port can be bound.
pub fn free_loopback_addr() -> Result<String> {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").wrap_err("failed to bind an ephemeral port")?;
    let addr = listener
        .local_addr()
        .wrap_err("failed to read the ephemeral port")?;
    Ok(addr.to_string())
}

/// Build a command for `bin` with a **cleared** environment plus exactly
/// `envs`, running in a fresh temporary directory.
///
/// Both halves matter. The binaries read `<SVC>_*` from the environment, and a
/// developer shell (or `sops exec-env`) may export real values: an inherited
/// `PBS_HOST` would point a test at production. The binaries also load `.env`
/// from their working directory via dotenvy, so running from the crate
/// directory could pick up a stray one. An empty directory has neither.
fn hermetic_command(
    bin: &str,
    envs: &[(&str, &str)],
    args: &[&str],
) -> Result<(tokio::process::Command, std::path::PathBuf)> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mcp-e2e-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)
        .wrap_err_with(|| format!("failed to create {}", dir.display()))?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.env_clear()
        .envs(envs.iter().copied())
        .args(args)
        .current_dir(&dir)
        .kill_on_drop(true);
    // Pass the CA bundle through: the Nix check sandbox has no /etc/ssl and
    // points reqwest at one with this variable (see `checks.test`). Harmless
    // elsewhere; it is not a `<SVC>_*` setting and cannot redirect a target.
    if let Ok(certs) = std::env::var("SSL_CERT_FILE") {
        cmd.env("SSL_CERT_FILE", certs);
    }
    Ok((cmd, dir))
}

/// Run `bin` to completion in a hermetic environment and return its output.
/// For the startup failure modes (missing or invalid config) and the
/// `--healthcheck` probe.
///
/// # Errors
///
/// Returns an error if the binary cannot be started, or does not exit within
/// [`STARTUP_TIMEOUT`], which for a config error means it is serving instead of
/// refusing to start.
pub async fn run_to_exit(
    bin: &str,
    envs: &[(&str, &str)],
    args: &[&str],
) -> Result<std::process::Output> {
    let (mut cmd, dir) = hermetic_command(bin, envs, args)?;
    let child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .wrap_err_with(|| format!("failed to spawn {bin}"))?;
    let output = tokio::time::timeout(STARTUP_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| eyre!("{bin} did not exit within {STARTUP_TIMEOUT:?}"))?
        .wrap_err_with(|| format!("failed waiting for {bin}"))?;
    let _ = std::fs::remove_dir_all(dir);
    Ok(output)
}

/// A server binary running as a child process, killed when dropped.
pub struct ServerProcess {
    child: tokio::process::Child,
    bind: String,
    dir: std::path::PathBuf,
}

impl ServerProcess {
    /// Spawn `bin` hermetically with `envs` and wait until it answers
    /// `/_healthcheck` on `bind`, which the caller must also have put in
    /// `envs` under the server's `*_BIND` name.
    ///
    /// # Errors
    ///
    /// Returns an error if the binary cannot be spawned, exits before becoming
    /// healthy (its stderr is included), or is not healthy within
    /// [`STARTUP_TIMEOUT`].
    pub async fn spawn(bin: &str, bind: &str, envs: &[(&str, &str)]) -> Result<Self> {
        let (mut cmd, dir) = hermetic_command(bin, envs, &[])?;
        let child = cmd
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .wrap_err_with(|| format!("failed to spawn {bin}"))?;
        let mut proc = Self {
            child,
            bind: bind.to_string(),
            dir,
        };

        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Some(status) = proc
                .child
                .try_wait()
                .wrap_err("failed to poll the server process")?
            {
                let stderr = match proc.child.stderr.take() {
                    Some(mut s) => {
                        let mut buf = String::new();
                        let _ = tokio::io::AsyncReadExt::read_to_string(&mut s, &mut buf).await;
                        buf
                    }
                    None => String::new(),
                };
                bail!("{bin} exited with {status} before becoming healthy:\n{stderr}");
            }
            if crate::run_healthcheck(bind).await.is_ok() {
                return Ok(proc);
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("{bin} was not healthy on {bind} within {STARTUP_TIMEOUT:?}");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// The address the process serves on.
    #[must_use]
    pub fn bind(&self) -> &str {
        &self.bind
    }

    /// The streamable-HTTP endpoint, `http://<bind>/mcp`.
    #[must_use]
    pub fn mcp_url(&self) -> String {
        format!("http://{}/mcp", self.bind)
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// How one server binary is configured, for [`check_binary_contract`].
pub struct BinarySpec<'a> {
    /// Path to the binary: `env!("CARGO_BIN_EXE_<name>")` in the test.
    pub bin: &'a str,
    /// The env var naming the listen address, e.g. `PBS_BIND`.
    pub bind_var: &'a str,
    /// Every env var the server refuses to start without, with a valid value.
    /// The contract removes each one in turn.
    pub required: &'a [(&'a str, &'a str)],
    /// Further env vars every run needs (optional settings under test).
    pub extra: &'a [(&'a str, &'a str)],
}

/// The startup contract every server binary must honour, checked against the
/// real executable in a hermetic environment. `Err` names the clause that
/// broke.
///
/// 1. **Each required variable, removed alone, stops startup** with a non-zero
///    exit whose stderr names that variable. The failure mode is a server
///    that starts half-configured and fails on the first call, or names the
///    wrong variable, so the operator fixes the wrong thing.
/// 2. **An occupied bind address stops startup**, naming the address. The
///    failure mode is a silent exit 0 that a supervisor reads as success.
/// 3. **Healthy means `--healthcheck` exits 0.** This is the container's own
///    probe: distroless has no shell or curl, so it is the only one there is.
/// 4. **`--healthcheck` against a dead address exits non-zero**, fast.
/// 5. **`tools/list` over the real socket is non-empty.** `main` wires the
///    same router the in-process tests use.
///
/// # Errors
///
/// Returns an error describing the first clause that failed.
pub async fn check_binary_contract(spec: &BinarySpec<'_>) -> Result<()> {
    let bind = free_loopback_addr()?;
    let mut base: Vec<(&str, &str)> = spec.required.to_vec();
    base.extend_from_slice(spec.extra);
    base.push((spec.bind_var, &bind));

    // 1. Missing required variables.
    for (missing, _) in spec.required {
        let envs: Vec<(&str, &str)> = base.iter().copied().filter(|(k, _)| k != missing).collect();
        let out = run_to_exit(spec.bin, &envs, &[]).await?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            bail!("{} started (exit 0) without {missing}", spec.bin);
        }
        if !stderr.contains(missing) {
            bail!(
                "{} failed without {missing}, but stderr does not name it:\n{stderr}",
                spec.bin
            );
        }
    }

    // 2. Occupied bind address.
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").wrap_err("failed to bind")?;
    let occupied_addr = occupied.local_addr().wrap_err("no local addr")?.to_string();
    let mut envs: Vec<(&str, &str)> = base
        .iter()
        .copied()
        .filter(|(k, _)| *k != spec.bind_var)
        .collect();
    envs.push((spec.bind_var, &occupied_addr));
    let out = run_to_exit(spec.bin, &envs, &[]).await?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.success() || !stderr.contains(&occupied_addr) {
        bail!(
            "{} with {} occupied: expected a failure naming it, got {} with:\n{stderr}",
            spec.bin,
            occupied_addr,
            out.status
        );
    }
    drop(occupied);

    // 3 + 5. Healthy, probes healthy, serves tools.
    let server = ServerProcess::spawn(spec.bin, &bind, &base).await?;
    let probe = run_to_exit(spec.bin, &[(spec.bind_var, &bind)], &["--healthcheck"]).await?;
    if !probe.status.success() {
        bail!(
            "--healthcheck against a healthy {} exited {}:\n{}",
            spec.bin,
            probe.status,
            String::from_utf8_lossy(&probe.stderr)
        );
    }
    let client = McpClient::connect(&server.mcp_url()).await?;
    if client.list_tools().await?.is_empty() {
        bail!("{} serves no tools", spec.bin);
    }
    drop(server);

    // 4. Dead address.
    let dead = free_loopback_addr()?;
    let probe = run_to_exit(spec.bin, &[(spec.bind_var, &dead)], &["--healthcheck"]).await?;
    if probe.status.success() {
        bail!("--healthcheck against dead {dead} exited 0");
    }
    Ok(())
}

// ---- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // The SSE framing is the one piece of the harness that every E2E test
    // trusts blindly. Its failure modes, written down first: a priming event
    // with no data; the response split over several `data:` lines; CRLF line
    // endings; a server notification ahead of the response; a final event with
    // no trailing blank line. Each gets a case below.

    fn sse(body: &str) -> RawResponse {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            reqwest::header::HeaderValue::from_static("text/event-stream"),
        );
        RawResponse {
            status: StatusCode::OK,
            headers,
            body: body.to_string(),
        }
    }

    #[test]
    fn sse_skips_priming_and_notifications_and_matches_the_id() {
        let raw = sse(concat!(
            "id: 0\nretry: 3000\ndata:\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{}}\n\n",
        ));
        let msg = extract_message(&raw, 7).unwrap();
        assert_eq!(msg, json!({ "jsonrpc": "2.0", "id": 7, "result": {} }));
    }

    #[test]
    fn sse_joins_multi_line_data_and_tolerates_crlf_and_no_trailing_blank() {
        let raw = sse("data: {\"jsonrpc\":\"2.0\",\r\ndata: \"id\":3,\"result\":1}");
        let msg = extract_message(&raw, 3).unwrap();
        assert_eq!(msg.get("result"), Some(&json!(1)));
    }

    #[test]
    fn a_response_for_another_id_is_an_error_not_a_match() {
        let raw = sse("data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n");
        assert!(extract_message(&raw, 2).is_err());
    }

    #[test]
    fn tool_error_is_never_readable_as_success_text() {
        let failed = ToolResult {
            message: json!({ "id": 1, "result": { "isError": true, "content": [{ "type": "text", "text": "boom" }] } }),
        };
        assert!(failed.text().is_err());
        assert_eq!(failed.error_message().unwrap(), "boom");
        assert_eq!(failed.transcript(), json!({ "tool_error": "boom" }));

        let rpc = ToolResult {
            message: json!({ "id": 1, "error": { "code": -32602, "message": "bad", "data": null } }),
        };
        assert!(rpc.text().is_err());
        assert_eq!(
            rpc.transcript(),
            json!({ "error": { "code": -32602, "message": "bad" } })
        );

        let ok = ToolResult {
            message: json!({ "id": 1, "result": { "content": [{ "type": "text", "text": "{\"a\":1}" }] } }),
        };
        assert!(ok.error_message().is_err());
        assert_eq!(ok.transcript(), json!({ "ok": { "a": 1 } }));
    }
}
