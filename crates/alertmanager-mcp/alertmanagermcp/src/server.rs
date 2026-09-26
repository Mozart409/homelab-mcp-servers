//! MCP server: exposes an Alertmanager instance as alert/silence inspection
//! tools, plus two optional silence-write tools behind a runtime gate.

use std::fmt::Write;

use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    Implementation, ListResourcesResult, PromptMessage, Role, ServerCapabilities, ServerConfig,
};
use rmcp::{
    ErrorData, ServerHandler, prompt, prompt_handler, prompt_router, schemars, tool, tool_handler,
    tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::{AlertmanagerClient, seg};

/// MCP server wrapping an [`AlertmanagerClient`].
#[derive(Clone)]
pub struct AlertmanagerServer {
    client: AlertmanagerClient,
    allow_silence: bool,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl AlertmanagerServer {
    /// Wrap a configured client as an MCP server.
    ///
    /// `allow_silence` decides whether the two mutating tools are registered at
    /// all. When it is `false` they are never merged into the router, so they do
    /// not appear in `tools/list` — see [`crate::Config::allow_silence`] for why
    /// absence is preferred over a tool that exists only to refuse.
    #[must_use]
    pub fn new(client: AlertmanagerClient, allow_silence: bool) -> Self {
        let mut tool_router = Self::read_tools();
        if allow_silence {
            tool_router.merge(Self::silence_tools());
        }

        Self {
            client,
            allow_silence,
            tool_router,
            prompt_router: Self::prompt_router(),
        }
    }

    /// Run a GET against the Alertmanager API and render the body as pretty JSON,
    /// mapping any error into an MCP error.
    async fn call(&self, path: &str, query: &[(&str, String)]) -> Result<String, ErrorData> {
        let data = self
            .client
            .get(path, query)
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        render(&data)
    }
}

/// Pretty-print a JSON value for return to the MCP client.
fn render(value: &Value) -> Result<String, ErrorData> {
    serde_json::to_string_pretty(value)
        .map_err(|e| ErrorData::internal_error(format!("failed to serialize response: {e}"), None))
}

/// Push an optional boolean filter as a query param.
fn push_bool(query: &mut Vec<(&'static str, String)>, key: &'static str, value: Option<bool>) {
    if let Some(v) = value {
        query.push((key, v.to_string()));
    }
}

/// Push repeated `filter` matchers and an optional `receiver` regex.
fn push_filters(
    query: &mut Vec<(&'static str, String)>,
    filter: Option<Vec<String>>,
    receiver: Option<String>,
) {
    if let Some(filters) = filter {
        for f in filters {
            query.push(("filter", f));
        }
    }
    if let Some(r) = receiver {
        query.push(("receiver", r));
    }
}

// ---- Tool parameter types ---------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListAlertsParams {
    /// Include alerts that are currently firing (default `true` server-side).
    #[serde(default)]
    active: Option<bool>,
    /// Include alerts suppressed by a silence (default `true` server-side).
    #[serde(default)]
    silenced: Option<bool>,
    /// Include alerts suppressed by an inhibition rule (default `true` server-side).
    #[serde(default)]
    inhibited: Option<bool>,
    /// Include alerts received but not yet processed into a group.
    #[serde(default)]
    unprocessed: Option<bool>,
    /// Label matchers to filter by, e.g. `["severity=\"critical\"", "job=~\"node.*\""]`.
    #[serde(default)]
    filter: Option<Vec<String>>,
    /// Regex matched against the receiver name, e.g. `webhook`.
    #[serde(default)]
    receiver: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AlertGroupsParams {
    /// Include alerts that are currently firing (default `true` server-side).
    #[serde(default)]
    active: Option<bool>,
    /// Include alerts suppressed by a silence (default `true` server-side).
    #[serde(default)]
    silenced: Option<bool>,
    /// Include alerts suppressed by an inhibition rule (default `true` server-side).
    #[serde(default)]
    inhibited: Option<bool>,
    /// Label matchers to filter by, e.g. `["severity=\"critical\""]`.
    #[serde(default)]
    filter: Option<Vec<String>>,
    /// Regex matched against the receiver name.
    #[serde(default)]
    receiver: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListSilencesParams {
    /// Label matchers to filter silences by, e.g. `["alertname=\"NodeDown\""]`.
    #[serde(default)]
    filter: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SilenceIdParams {
    /// The silence's ID, as returned by `list_silences` or `create_silence`.
    id: String,
}

/// One label matcher in a silence definition.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SilenceMatcher {
    /// Label name to match, e.g. `alertname`.
    name: String,
    /// Value to match against. Treated as a regex when `is_regex` is true.
    value: String,
    /// Match `value` as a regular expression rather than a literal.
    #[serde(default)]
    is_regex: Option<bool>,
    /// Match equality (`true`, the default) or inequality (`false`).
    #[serde(default)]
    is_equal: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateSilenceParams {
    /// Matchers selecting which alerts to suppress. An empty list would silence
    /// every alert, so at least one is required.
    matchers: Vec<SilenceMatcher>,
    /// When the silence ends: RFC3339, e.g. `2026-09-07T18:00:00Z`. Required —
    /// an open-ended silence is indefinite alert blindness.
    ends_at: String,
    /// When the silence starts: RFC3339. Defaults to now.
    #[serde(default)]
    starts_at: Option<String>,
    /// Who is creating the silence, recorded in Alertmanager's audit trail.
    created_by: String,
    /// Why the alerts are being suppressed.
    comment: String,
}

// ---- Prompt arguments -------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct NotificationAuditArgs {
    /// Restrict the audit to a single alert name, e.g. `NodeDown`. Omit to audit all.
    #[serde(default)]
    alertname: Option<String>,
    /// Restrict the audit to alerts routed to a given receiver, e.g. `webhook`.
    #[serde(default)]
    receiver: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SilenceReviewArgs {
    /// Label matcher narrowing which silences to review, e.g. `job="node"`.
    #[serde(default)]
    filter: Option<String>,
}

// ---- Read-only tools --------------------------------------------------------

#[tool_router(router = read_tools)]
impl AlertmanagerServer {
    #[tool(
        description = "List alerts currently known to Alertmanager, with their labels, annotations, receivers, and whether each is silenced or inhibited. Filter by state and by label matchers."
    )]
    async fn list_alerts(
        &self,
        Parameters(ListAlertsParams {
            active,
            silenced,
            inhibited,
            unprocessed,
            filter,
            receiver,
        }): Parameters<ListAlertsParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        push_bool(&mut q, "active", active);
        push_bool(&mut q, "silenced", silenced);
        push_bool(&mut q, "inhibited", inhibited);
        push_bool(&mut q, "unprocessed", unprocessed);
        push_filters(&mut q, filter, receiver);
        self.call("/api/v2/alerts", &q).await
    }

    #[tool(
        description = "List alerts grouped the way Alertmanager's routing tree groups them, with the receiver each group resolves to. Use this rather than list_alerts when the question is about routing or notification batching."
    )]
    async fn alert_groups(
        &self,
        Parameters(AlertGroupsParams {
            active,
            silenced,
            inhibited,
            filter,
            receiver,
        }): Parameters<AlertGroupsParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        push_bool(&mut q, "active", active);
        push_bool(&mut q, "silenced", silenced);
        push_bool(&mut q, "inhibited", inhibited);
        push_filters(&mut q, filter, receiver);
        self.call("/api/v2/alerts/groups", &q).await
    }

    #[tool(
        description = "List silences with their matchers, time window, creator, comment, and state (active, pending, or expired)."
    )]
    async fn list_silences(
        &self,
        Parameters(ListSilencesParams { filter }): Parameters<ListSilencesParams>,
    ) -> Result<String, ErrorData> {
        let mut q = Vec::new();
        push_filters(&mut q, filter, None);
        self.call("/api/v2/silences", &q).await
    }

    #[tool(description = "Get a single silence by its ID.")]
    async fn get_silence(
        &self,
        Parameters(SilenceIdParams { id }): Parameters<SilenceIdParams>,
    ) -> Result<String, ErrorData> {
        // Singular `/silence/{id}` here, plural `/silences` for the list — the
        // Alertmanager v2 API really does spell these two differently.
        self.call(&format!("/api/v2/silence/{}", seg(&id)?), &[])
            .await
    }

    #[tool(
        description = "List the configured receiver names. Use this to learn what notification destinations exist before tracing where an alert was routed."
    )]
    async fn list_receivers(&self) -> Result<String, ErrorData> {
        self.call("/api/v2/receivers", &[]).await
    }

    #[tool(
        description = "Get Alertmanager's status: version info, uptime, cluster peers and status, and the currently loaded configuration."
    )]
    async fn status(&self) -> Result<String, ErrorData> {
        self.call("/api/v2/status", &[]).await
    }
}

// ---- Silence-write tools (gated) -------------------------------------------

/// The mutating half of the tool surface.
///
/// This block is registered only when `ALERTMANAGER_ALLOW_SILENCE` is set; see
/// [`AlertmanagerServer::new`]. It is kept in its own `impl` so the gate is a
/// single `merge` call rather than a condition threaded through each tool.
#[tool_router(router = silence_tools)]
impl AlertmanagerServer {
    #[tool(
        description = "Create a silence suppressing alerts that match the given label matchers, until ends_at. Requires at least one matcher and an explicit end time."
    )]
    async fn create_silence(
        &self,
        Parameters(CreateSilenceParams {
            matchers,
            ends_at,
            starts_at,
            created_by,
            comment,
        }): Parameters<CreateSilenceParams>,
    ) -> Result<String, ErrorData> {
        // A silence with no matchers matches every alert. Alertmanager accepts
        // that; refusing here means the destructive default cannot be reached by
        // omitting an argument.
        if matchers.is_empty() {
            return Err(ErrorData::invalid_params(
                "at least one matcher is required — a silence with no matchers would suppress every alert".to_string(),
                None,
            ));
        }

        let starts_at = starts_at.unwrap_or_else(|| {
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        });

        let matchers: Vec<Value> = matchers
            .into_iter()
            .map(|m| {
                json!({
                    "name": m.name,
                    "value": m.value,
                    "isRegex": m.is_regex.unwrap_or(false),
                    "isEqual": m.is_equal.unwrap_or(true),
                })
            })
            .collect();

        let body = json!({
            "matchers": matchers,
            "startsAt": starts_at,
            "endsAt": ends_at,
            "createdBy": created_by,
            "comment": comment,
        });

        let data = self
            .client
            .post_json("/api/v2/silences", &body)
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;
        render(&data)
    }

    #[tool(
        description = "Expire a silence by ID, ending it immediately so its alerts resume notifying. Alertmanager has no delete — an expired silence remains visible in list_silences."
    )]
    async fn expire_silence(
        &self,
        Parameters(SilenceIdParams { id }): Parameters<SilenceIdParams>,
    ) -> Result<String, ErrorData> {
        self.client
            .delete(&format!("/api/v2/silence/{}", seg(&id)?))
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))?;

        Ok(format!("silence {id} expired"))
    }
}

// ---- Prompts ----------------------------------------------------------------

/// Repeated Alertmanager workflows, encoded as prompts.
///
/// These live in their own inherent `impl` block so `#[prompt_router]` and each
/// `#[tool_router]` own one block outright — the macros each generate an
/// associated router constructor, and keeping them separate avoids asking any of
/// them to walk attributes it does not recognise.
#[prompt_router]
impl AlertmanagerServer {
    /// Trace an alert from firing through grouping and routing to a receiver,
    /// separating suppression from delivery failure.
    #[prompt(
        name = "notification_audit",
        description = "Determine why an alert did or did not notify: trace it through grouping and routing to a receiver, and distinguish silenced from inhibited from delivered."
    )]
    async fn notification_audit(
        &self,
        params: Parameters<NotificationAuditArgs>,
    ) -> Vec<PromptMessage> {
        let NotificationAuditArgs {
            alertname,
            receiver,
        } = params.0;

        let mut instructions = String::from(
            "Audit what Alertmanager did with the current alerts, and tell me which ones \
             actually reached a human.\n\n\
             Work in this order:\n\
             1. Call `list_alerts` with no state filters so suppressed alerts are included. ",
        );

        if let Some(a) = &alertname {
            let _ = write!(instructions, "Restrict to alerts named `{a}`. ");
        }
        if let Some(r) = &receiver {
            let _ = write!(
                instructions,
                "Restrict to alerts routed to receiver `{r}`. "
            );
        }

        instructions.push_str(
            "For each alert note its `status.state` and, critically, its `status.silencedBy`, \
             `status.inhibitedBy`, and `status.mutedBy` arrays — those are what separate a \
             suppressed alert from a delivered one. All three suppress, but have different causes \
             and different fixes: a silence is deliberate and has an owner, an inhibition is a rule \
             saying some other alert supersedes this one, and a mute is a time interval \
             (out-of-hours, maintenance window) configured in the routing tree.\n\
             2. Call `alert_groups` for the same filters. This is the only view that shows the \
             routing tree's grouping and the receiver each group resolved to; `list_alerts` alone \
             cannot tell you where a notification was sent.\n\
             3. For every non-empty `silencedBy`, call `get_silence` on that ID to recover who \
             created it, why, and when it expires. A silence nobody remembers creating is the most \
             common cause of an alert that 'should have paged'.\n\
             4. Call `list_receivers` and `status` to confirm the receiver actually exists in the \
             loaded config and that Alertmanager is not in a degraded cluster state.\n\n\
             Report:\n\
             - Split the alerts into four groups: delivered, suppressed by silence, suppressed by \
             inhibition, muted by a time interval. Never merge them — they have different fixes, \
             and a muted alert in particular looks like nothing is wrong at all.\n\
             - For each suppressed alert, name the silence, inhibition rule, or time interval \
             responsible, and quote the silence's comment and expiry where there is one.\n\
             - Flag any alert whose group resolved to a receiver that does not appear in \
             `list_receivers` — that is a routing misconfiguration, not a suppression.\n\
             - Do not infer routing from alert labels; read it from `alert_groups`."
        );

        vec![PromptMessage::new_text(Role::User, instructions)]
    }

    /// Audit silences for over-broad matchers and for expiries that will unmask
    /// alerts that are still firing.
    #[prompt(
        name = "silence_review",
        description = "Review active and pending silences: flag over-broad matchers, stale silences, and ones about to expire on alerts that are still firing."
    )]
    async fn silence_review(&self, params: Parameters<SilenceReviewArgs>) -> Vec<PromptMessage> {
        let SilenceReviewArgs { filter } = params.0;

        let mut instructions = String::from(
            "Review the silences configured in this Alertmanager and tell me which ones are \
             hiding problems.\n\n\
             Work in this order:\n\
             1. Call `list_silences` to get every silence with its matchers, window, creator, and \
             comment. ",
        );

        if let Some(f) = &filter {
            let _ = write!(instructions, "Restrict to silences matching `{f}`. ");
        }

        instructions.push_str(
            "Separate them by state: active, pending, expired. Silence IDs are UUIDs; pass one \
             to `get_silence` verbatim.\n\
             2. Call `list_alerts` with `silenced=true` to see which alerts each active silence is \
             actually suppressing right now. A silence suppressing nothing is either stale or \
             mistargeted; a silence suppressing far more than its comment implies is over-broad.\n\
             3. For each active silence, judge the matchers. A regex matcher on `alertname`, or a \
             silence with a single matcher on a broad label like `job` or `instance`, suppresses \
             far more than it usually intends. Quote the matcher when you flag it.\n\
             4. Cross-reference expiry times against what is still firing: a silence about to \
             expire on an alert that is still active means a notification is about to arrive, \
             possibly at an inconvenient hour.\n\n\
             Report:\n\
             - Silences that are suppressing nothing (candidates for removal).\n\
             - Silences whose matchers are broader than their stated comment justifies, with the \
             count of alerts each is actually hiding.\n\
             - Silences expiring soon whose alerts are still firing, so the operator can decide to \
             extend or to fix the underlying issue first.\n\
             - Anything silenced without a comment or with a placeholder comment — that is an audit \
             trail failure regardless of whether the silence is correct."
        );

        vec![PromptMessage::new_text(Role::User, instructions)]
    }
}

// Rust 1.98's `clippy::unused_async_trait_impl` (pedantic, therefore deny here)
// fires four times on this block, and only two of them are ours: `list_resources`
// and `read_resource` genuinely have no `.await`. The other two originate inside
// the `tool_handler` and `prompt_handler` expansions -- rmcp generates async trait
// methods whose bodies are `std::future::ready(..)` -- so there is no source in
// this repo to change. Silencing it per-method would still leave the macro pair
// failing, which is why the allow sits on the whole impl. Revisit when rmcp stops
// generating bodies that never await; the two hand-written methods can drop their
// `async` at that point.
#[allow(clippy::unused_async_trait_impl)]
#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for AlertmanagerServer {
    fn get_info(&self) -> ServerConfig {
        // `ServerConfig` is `#[non_exhaustive]`, so build from default and assign.
        let mut info = ServerConfig::default();

        // State the write posture in the instructions. A client that cannot see
        // `create_silence` should be told the capability is gated off rather than
        // left to conclude this Alertmanager has no silence support at all.
        let posture = if self.allow_silence {
            "Silence creation and expiry are ENABLED on this instance \
             (ALERTMANAGER_ALLOW_SILENCE is set): `create_silence` and `expire_silence` will \
             change what notifies. Every other tool is read-only."
        } else {
            "This server is read-only. Silence creation and expiry are gated off \
             (ALERTMANAGER_ALLOW_SILENCE is unset), so `create_silence` and `expire_silence` are \
             not registered — report silences as something the operator must change by hand."
        };

        info.instructions = Some(format!(
            "Access to an Alertmanager instance: inspect alerts as Alertmanager sees them \
             (including why they are suppressed), the routing groups and receivers they resolve \
             to, and the silences in effect. Prometheus can tell you an alert is firing; use these \
             tools for what happened to the notification afterwards. {posture}"
        ));

        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_prompts()
            .enable_resources()
            .build();

        let mut server_info = Implementation::default();
        server_info.name = env!("CARGO_PKG_NAME").to_string();
        server_info.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info = server_info;

        info
    }

    /// Advertise this crate's README as the server's operator guide.
    ///
    /// The README carries the matcher syntax, the silence lifecycle, and the
    /// v2 API caveats that no tool return value contains, so exposing it as a
    /// resource lets a client read the reasoning without spending a tool call.
    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let uri = mcp_common::doc_resource_uri(env!("CARGO_PKG_NAME"));
        Ok(ListResourcesResult::with_all_items(vec![
            mcp_common::doc_resource(
                &uri,
                "Alertmanager MCP operator guide",
                "README for alertmanager-mcp: matcher syntax, silence lifecycle, and API caveats.",
            ),
        ]))
    }

    /// Serve the operator guide's markdown for the URI advertised above.
    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, ErrorData> {
        let uri = mcp_common::doc_resource_uri(env!("CARGO_PKG_NAME"));
        if request.uri == uri {
            Ok(mcp_common::doc_resource_contents(&uri, include_str!("../../README.md")).into())
        } else {
            Err(ErrorData::resource_not_found(
                format!("unknown resource uri: {}", request.uri),
                None,
            ))
        }
    }
}
