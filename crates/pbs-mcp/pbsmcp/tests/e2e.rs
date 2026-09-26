//! End-to-end tests: the production router on a real socket, driven over MCP,
//! against a wiremock stand-in for the PBS REST API.
//!
//! Failure modes this suite exists to catch (written before the tests):
//!
//! - a tool hits the wrong path, or percent-encodes a UPID (`:` and `@`) or a
//!   nested namespace wrongly, so PBS returns 404 for something that exists;
//! - an optional filter the caller left out still reaches PBS as an empty
//!   param, which PBS treats differently from "absent";
//! - `task_log` with `tail` fetches the wrong window: off by one, or an
//!   underflow when the log is shorter than `tail`, or `total` taken from the
//!   stale probe instead of the final fetch;
//! - `tail` together with `start` is silently accepted and one of them ignored;
//! - an upstream 401 / 500-with-HTML / truncated JSON / dead host turns into a
//!   hang, a panic, or a "successful" empty answer instead of a tool error;
//! - one failed call poisons the MCP session, so the next call fails too;
//! - any tool sends anything but GET (hard rule §1);
//! - a DNS-rebinding `Host` header is accepted, including when the configured
//!   allow-list is empty (rmcp reads empty as "allow all");
//! - the advertised contract (tool names, schemas, prompts, the doc resource)
//!   drifts without anyone reviewing it.
//!
//! Artifacts: every scenario's call → upstream → response transcript, and the
//! full `tools/list` contract, are committed snapshots under `tests/snapshots/`.

use color_eyre::eyre::Result;
use mcp_common::e2e::{self, McpClient, Scenario, TestServer, method_surface};
use pbsmcp::Config;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "mcp@pbs!homelab:2f9c1d7e-5b8a-4c3f-9e21-7a6b5c4d3e2f";
const NODE: &str = "localhost";
const FAILED_UPID: &str =
    "UPID:pbs01:0000A1B2:0012F3C4:00000007:66F1A2B3:backup:nas-01\\x3avm-101:root@pam:";

fn config(base_url: &str) -> Config {
    Config {
        base_url: base_url.to_string(),
        api_key: API_KEY.to_string(),
        node: NODE.to_string(),
        insecure: false,
        bind: "127.0.0.1:0".to_string(),
        allowed_hosts: None,
    }
}

async fn serve(config: &Config) -> Result<(TestServer, McpClient)> {
    let server = TestServer::start(pbsmcp::router(config)?).await?;
    let client = server.connect().await?;
    Ok((server, client))
}

fn pbs(data: &Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "data": data }))
}

// ---- Contract ---------------------------------------------------------------

#[tokio::test]
async fn contract() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    insta::assert_json_snapshot!("contract", e2e::contract(&client).await.unwrap());
    e2e::check_doc_resource(
        &client,
        "doc://pbsmcp/guide",
        include_str!("../../README.md"),
    )
    .await
    .unwrap();
    // Nothing above may touch PBS.
    assert!(mock.received_requests().await.unwrap().is_empty());
}

// ---- Read-only surface ------------------------------------------------------

#[tokio::test]
async fn every_tool_sends_only_get_requests() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    let surface = method_surface(&client, &mock, &json!({ "data": [], "total": 0 }))
        .await
        .unwrap();
    insta::assert_json_snapshot!("method_surface", surface);

    for (tool, methods) in surface.as_object().unwrap() {
        assert_eq!(methods, &json!(["GET"]), "{tool} must only GET");
    }
}

// ---- Scenarios --------------------------------------------------------------

/// An operator asking "why did last night's backup of VM 101 fail, and is the
/// store filling up?": the full multi-tool investigation an LLM would run, in
/// one session, including a 1200-line task log read from the tail.
#[tokio::test]
// One investigation, read top to bottom; splitting it would hide the story.
#[allow(clippy::too_many_lines)]
async fn failed_backup_investigation() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api2/json/admin/datastore"))
        .respond_with(pbs(&json!([
            { "store": "nas-01", "comment": "primary, ZFS mirror" },
            { "store": "offsite", "comment": "synced to B2 nightly", "maintenance": "read-only" }
        ])))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api2/json/admin/datastore/nas-01/status"))
        .respond_with(pbs(&json!({
            "total": 3_985_729_650_688_u64,
            "used": 3_788_121_456_640_u64,
            "avail": 197_608_194_048_u64,
            "estimated-full-date": 1_727_654_400,
            "gc-status": {
                "upid": "UPID:pbs01:00001F00:00AB0000:00000000:66F0C000:garbage_collection:nas-01:root@pam:",
                "index-file-count": 1843,
                "removed-bytes": 0,
                "pending-bytes": 412_316_860_416_u64,
                "disk-chunks": 1_204_551
            },
            "counts": { "vm": { "groups": 12, "snapshots": 341 }, "ct": { "groups": 7, "snapshots": 198 } }
        })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api2/json/admin/gc"))
        .respond_with(pbs(&json!([
            { "store": "nas-01", "schedule": "daily", "last-run-state": "error: unable to acquire lock", "last-run-endtime": 1_727_222_400, "next-run": 1_727_308_800 },
            { "store": "offsite", "schedule": "sat 02:00", "last-run-state": "ok", "last-run-endtime": 1_726_963_200, "next-run": 1_727_568_000 }
        ])))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api2/json/nodes/localhost/tasks"))
        .respond_with(pbs(&json!([
            {
                "upid": FAILED_UPID, "node": "pbs01", "pid": 41394, "starttime": 1_727_226_035,
                "endtime": 1_727_226_977, "worker_type": "backup", "worker_id": "nas-01:vm/101",
                "user": "root@pam", "status": "backup failed: connection error: timed out"
            }
        ])))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/api2/json/nodes/localhost/tasks/[^/]+/status$",
        ))
        .respond_with(pbs(&json!({
            "upid": FAILED_UPID, "node": "pbs01", "pid": 41394, "type": "backup",
            "id": "nas-01:vm/101", "user": "root@pam", "starttime": 1_727_226_035,
            "status": "stopped", "exitstatus": "backup failed: connection error: timed out"
        })))
        .mount(&mock)
        .await;
    // The log is 1200 lines. The probe (limit=1) and the window fetch are
    // told apart by their query, as PBS would answer them.
    Mock::given(method("GET"))
        .and(path_regex(r"^/api2/json/nodes/localhost/tasks/[^/]+/log$"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "n": 1, "t": "starting new backup on datastore 'nas-01': \"vm/101/2024-09-25T01:00:35Z\"" }],
            "total": 1200
        })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api2/json/nodes/localhost/tasks/[^/]+/log$"))
        .and(query_param("start", "1197"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "n": 1198, "t": "upload_chunk: 4.00 MiB in 30.01s (0.13 MiB/s)" },
                { "n": 1199, "t": "backup ended: connection error: timed out" },
                { "n": 1200, "t": "TASK ERROR: backup failed: connection error: timed out" }
            ],
            "total": 1200
        })))
        .mount(&mock)
        .await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);

    s.call("list_datastores", json!({}))
        .await
        .unwrap()
        .text()
        .unwrap();
    s.call("datastore_status", json!({ "store": "nas-01" }))
        .await
        .unwrap()
        .text()
        .unwrap();
    s.call("gc_status", json!({}))
        .await
        .unwrap()
        .text()
        .unwrap();
    s.call(
        "list_tasks",
        json!({ "errors_only": true, "since": 1_727_222_400, "limit": 20 }),
    )
    .await
    .unwrap()
    .text()
    .unwrap();
    s.call("task_status", json!({ "upid": FAILED_UPID }))
        .await
        .unwrap()
        .text()
        .unwrap();
    let log = s
        .call("task_log", json!({ "upid": FAILED_UPID, "tail": 3 }))
        .await
        .unwrap()
        .json()
        .unwrap();

    // The window is exactly the last three lines, and says where it starts.
    assert_eq!(log.get("start"), Some(&json!(1197)));
    assert_eq!(log.get("total"), Some(&json!(1200)));
    insta::assert_json_snapshot!("failed_backup_investigation", s.transcript());
}

/// Snapshot listing with every filter set, in a nested namespace: the query
/// must carry `ns`, `backup-type`, `backup-id` under PBS's own spellings.
#[tokio::test]
async fn snapshot_listing_with_every_filter_in_a_nested_namespace() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api2/json/admin/datastore/nas-01/snapshots"))
        .respond_with(pbs(&json!([
            {
                "backup-type": "vm", "backup-id": "101", "backup-time": 1_727_226_035,
                "size": 34_359_738_368_u64, "owner": "root@pam", "protected": false,
                "verification": { "state": "ok", "upid": "UPID:pbs01:00002A00:00BC0000:00000000:66F2D000:verify:nas-01:root@pam:" },
                "files": [ { "filename": "drive-scsi0.img.fidx", "size": 34_359_738_368_u64, "crypt-mode": "encrypt" } ]
            },
            {
                "backup-type": "vm", "backup-id": "101", "backup-time": 1_727_139_635,
                "size": 34_359_738_368_u64, "owner": "root@pam", "protected": true,
                "verification": { "state": "failed", "upid": "UPID:pbs01:00002B00:00BD0000:00000000:66F1E000:verify:nas-01:root@pam:" }
            }
        ])))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api2/json/nodes/localhost/tasks"))
        .respond_with(pbs(&json!([])))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api2/json/admin/datastore/nas-01/groups"))
        .respond_with(pbs(&json!([
            { "backup-type": "vm", "backup-id": "101", "last-backup": 1_727_226_035, "backup-count": 14, "owner": "root@pam", "comment": "gitea" }
        ])))
        .mount(&mock)
        .await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    s.call(
        "list_groups",
        json!({ "store": "nas-01", "namespace": "prod/web" }),
    )
    .await
    .unwrap()
    .text()
    .unwrap();
    s.call(
        "list_snapshots",
        json!({
            "store": "nas-01", "namespace": "prod/web", "backup_type": "vm", "backup_id": "101"
        }),
    )
    .await
    .unwrap()
    .text()
    .unwrap();
    // No filters at all: no query string, not `?ns=`.
    s.call("list_snapshots", json!({ "store": "nas-01" }))
        .await
        .unwrap()
        .text()
        .unwrap();
    // No arguments: the default limit of 50 and no `errors`/`running`/`since`.
    s.call("list_tasks", json!({}))
        .await
        .unwrap()
        .text()
        .unwrap();

    insta::assert_json_snapshot!("snapshot_listing", s.transcript());
}

/// `tail` longer than the log itself: the window must start at 0, not
/// underflow, and `total` must come from the final fetch. The log grew between
/// the probe and the fetch, as it does for a running task.
#[tokio::test]
async fn task_log_tail_longer_than_a_growing_log() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"/log$"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "n": 1, "t": "starting garbage collection on store nas-01" }], "total": 2
        })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"/log$"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "n": 1, "t": "starting garbage collection on store nas-01" },
                { "n": 2, "t": "Start GC phase1 (mark used chunks)" },
                { "n": 3, "t": "marked 1% (18 of 1843 index files)" }
            ],
            "total": 3
        })))
        .mount(&mock)
        .await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    let upid = "UPID:pbs01:00001F00:00AB0000:00000000:66F0C000:garbage_collection:nas-01:root@pam:";
    let log = s
        .call("task_log", json!({ "upid": upid, "tail": 50 }))
        .await
        .unwrap()
        .json()
        .unwrap();

    assert_eq!(log.get("start"), Some(&json!(0)));
    assert_eq!(
        log.get("total"),
        Some(&json!(3)),
        "total must be the fresh one"
    );
    insta::assert_json_snapshot!("task_log_tail_longer_than_log", s.transcript());
}

#[tokio::test]
async fn task_log_refuses_tail_with_start_without_calling_pbs() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    let res = client
        .call_tool(
            "task_log",
            json!({ "upid": FAILED_UPID, "tail": 10, "start": 5 }),
        )
        .await
        .unwrap();

    assert!(res.error_message().unwrap().contains("mutually exclusive"));
    assert!(mock.received_requests().await.unwrap().is_empty());
}

/// IDs are caller-controlled. `..` must be refused, not sent: the URL parser
/// normalises it (even as `%2E%2E`) and the request would reach a different
/// endpoint, here `/nodes/localhost/log` instead of a task's log.
#[tokio::test]
async fn dot_segment_ids_are_refused_before_reaching_pbs() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);

    for (tool, args) in [
        ("task_log", json!({ "upid": ".." })),
        ("task_status", json!({ "upid": "." })),
        ("datastore_status", json!({ "store": "" })),
        ("list_snapshots", json!({ "store": ".." })),
    ] {
        let res = s.call(tool, args).await.unwrap();
        assert!(
            res.error_message()
                .unwrap()
                .contains("not a valid path segment")
        );
    }
    assert!(mock.received_requests().await.unwrap().is_empty());
    insta::assert_json_snapshot!("dot_segments_refused", s.transcript());
}

// ---- Upstream failures ------------------------------------------------------

/// Four ways PBS can fail, each followed by a healthy call on the same
/// session. Every failure must be a tool error naming the cause, and must not
/// take the session down with it.
#[tokio::test]
async fn upstream_failures_are_tool_errors_and_the_session_survives() {
    let mock = MockServer::start().await;
    Mock::given(path("/api2/json/admin/datastore/locked/status"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "data": null, "message": "authentication failure\n"
        })))
        .mount(&mock)
        .await;
    Mock::given(path("/api2/json/admin/datastore/broken/status"))
        .respond_with(ResponseTemplate::new(500).set_body_raw(
            "<html><head><title>500 Internal Server Error</title></head><body>proxy error</body></html>",
            "text/html",
        ))
        .mount(&mock)
        .await;
    Mock::given(path("/api2/json/admin/datastore/truncated/status"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(r#"{"data":{"total":39857"#, "application/json"),
        )
        .mount(&mock)
        .await;
    Mock::given(path("/api2/json/admin/gc"))
        .respond_with(pbs(&json!([])))
        .mount(&mock)
        .await;
    // A task-log envelope without `total` (a proxy that rewrote the body, or a
    // PBS change): the tail window cannot be computed and must not be guessed.
    Mock::given(path_regex(r"/log$"))
        .respond_with(pbs(&json!([{ "n": 1, "t": "starting" }])))
        .mount(&mock)
        .await;

    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();
    let mut s = Scenario::new(&client, &mock);
    for store in ["locked", "broken", "truncated"] {
        let res = s
            .call("datastore_status", json!({ "store": store }))
            .await
            .unwrap();
        assert!(!res.is_success(), "{store} must fail");
        s.call("gc_status", json!({}))
            .await
            .unwrap()
            .text()
            .unwrap();
    }
    let res = s
        .call("task_log", json!({ "upid": FAILED_UPID, "tail": 5 }))
        .await
        .unwrap();
    assert!(res.error_message().unwrap().contains("total"));
    s.call("gc_status", json!({}))
        .await
        .unwrap()
        .text()
        .unwrap();
    insta::assert_json_snapshot!("upstream_failures", s.transcript());
}

#[tokio::test]
async fn unreachable_pbs_is_a_prompt_tool_error() {
    // A port that was free a moment ago: connection refused, immediately.
    let dead = format!("http://{}", mcp_common::e2e::free_loopback_addr().unwrap());
    let (_server, client) = serve(&config(&dead)).await.unwrap();

    let started = std::time::Instant::now();
    let res = client
        .call_tool("list_datastores", json!({}))
        .await
        .unwrap();
    let msg = res.error_message().unwrap();

    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "must not hang"
    );
    assert!(
        msg.contains("/api2/json/admin/datastore"),
        "names the request: {msg}"
    );
}

// ---- Transport security -----------------------------------------------------

#[tokio::test]
async fn dns_rebinding_guard() {
    let mock = MockServer::start().await;
    let uri = mock.uri();
    e2e::check_host_allow_list(|allowed_hosts| {
        let mut cfg = config(&uri);
        cfg.allowed_hosts = allowed_hosts;
        pbsmcp::router(&cfg)
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn insecure_flag_is_applied_both_ways() {
    let mock = MockServer::start().await;
    Mock::given(path("/api2/json/admin/gc"))
        .respond_with(pbs(&json!([])))
        .mount(&mock)
        .await;
    e2e::check_insecure_flag(
        &mock,
        |base_url, insecure| {
            let mut cfg = config(base_url);
            cfg.insecure = insecure;
            pbsmcp::router(&cfg)
        },
        "gc_status",
        json!({}),
    )
    .await
    .unwrap();
}

/// A misspelt argument is refused before anything is sent upstream, instead
/// of being dropped so that the call quietly answers a different question.
#[tokio::test]
async fn unknown_arguments_are_refused() {
    let mock = MockServer::start().await;
    let (_server, client) = serve(&config(&mock.uri())).await.unwrap();

    let refusals = e2e::unknown_arguments(&client, Some(&mock)).await.unwrap();
    insta::assert_json_snapshot!("unknown_arguments", refusals);
}
