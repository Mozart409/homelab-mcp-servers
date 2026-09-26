//! End-to-end tests: the production router on a real socket, driven over MCP,
//! against a **real PostgreSQL**. Nothing here is mocked or skipped.
//!
//! `just test` (and the `checks.test` Nix derivation) start a throwaway,
//! durability-off cluster via `scripts/test-pg.sh` and export
//! `PGMCP_TEST_DATABASE_URL`. Every test creates its **own database** from it,
//! seeds it, and drops it, so tests run in parallel without seeing each other.
//! Running `cargo test` without the wrapper fails loudly: an unverified
//! read-only guarantee must never look like a passing one.
//!
//! Failure modes this suite exists to catch (written before the tests):
//!
//! - **a write gets through `run_query`** by any shape: plain DML, DDL,
//!   `TRUNCATE`, a data-modifying CTE, `nextval()`, `SELECT … FOR UPDATE`, a
//!   volatile function that writes (a *valid* SELECT — the only shape the
//!   subquery wrapper's syntax cannot reject, so the one that proves the READ
//!   ONLY transaction itself),
//!   flipping `transaction_read_only` off from inside the query, stacking a
//!   second statement after `;`, or closing the wrapper's parenthesis to
//!   escape the subquery. Each must fail, and the database must be provably
//!   unchanged afterwards;
//! - **the statement timeout is bypassed**, including by the query itself
//!   calling `set_config('statement_timeout', '0', …)`;
//! - **the row cap is bypassed** by omitting `limit` or asking for more, or a
//!   degenerate `limit` (0, negative) errors or panics instead of clamping;
//! - **an introspection argument is interpolated** rather than bound, so a
//!   table name carrying SQL executes it;
//! - **types render wrongly** as JSON: numeric precision, timestamptz, arrays,
//!   jsonb, NULL, non-ASCII text;
//! - **concurrent sessions exhaust or deadlock the pool**;
//! - **an unreachable database** hangs a call instead of failing it, or stops
//!   the server from starting (the pool is lazy by design);
//! - the advertised contract drifts unreviewed.
//!
//! Artifacts: committed snapshots of the contract, the introspection
//! transcript, the type rendering, and the refused-write catalogue.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr, eyre};
use mcp_common::e2e::{McpClient, TestServer, ToolResult};
use pgmcp::Config;
use serde_json::{Value, json};
use sqlx::PgPool;

/// The cluster's superuser URL, `postgres://…/postgres?host=<socket dir>`.
fn admin_url() -> Result<String> {
    std::env::var("PGMCP_TEST_DATABASE_URL").map_err(|_| {
        eyre!(
            "PGMCP_TEST_DATABASE_URL is not set. pgmcp's tests need a real Postgres: \
             run them with `just test` (or `scripts/test-pg.sh run -- cargo test -p pgmcp`), \
             which starts a throwaway cluster"
        )
    })
}

/// `url` with its database name replaced by `db`.
fn with_database(url: &str, db: &str) -> Result<String> {
    let (head, query) = url.split_once('?').unwrap_or((url, ""));
    let base = head
        .rsplit_once('/')
        .map(|(b, _)| b)
        .ok_or_else(|| eyre!("no database path in {url}"))?;
    Ok(if query.is_empty() {
        format!("{base}/{db}")
    } else {
        format!("{base}/{db}?{query}")
    })
}

/// A database of its own for one test, seeded with [`SEED`], dropped on
/// [`TestDb::drop_db`]. `admin` stays connected to it for before/after checks
/// that must not go through the server under test.
struct TestDb {
    name: String,
    url: String,
    admin: PgPool,
    cluster: PgPool,
}

static SEQ: AtomicU32 = AtomicU32::new(0);

impl TestDb {
    async fn create() -> Result<Self> {
        let admin_url = admin_url()?;
        let name = format!(
            "pgmcp_e2e_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let cluster = PgPool::connect(&admin_url)
            .await
            .wrap_err("connecting to the test cluster")?;
        // Test-generated identifier, never caller input.
        let create = format!("CREATE DATABASE {name}");
        sqlx::raw_sql(sqlx::AssertSqlSafe(create))
            .execute(&cluster)
            .await
            .wrap_err("CREATE DATABASE")?;
        let url = with_database(&admin_url, &name)?;
        let admin = PgPool::connect(&url)
            .await
            .wrap_err("connecting to test db")?;
        sqlx::raw_sql(SEED)
            .execute(&admin)
            .await
            .wrap_err("seeding")?;
        Ok(Self {
            name,
            url,
            admin,
            cluster,
        })
    }

    fn config(&self) -> Config {
        Config {
            database_url: self.url.clone(),
            bind: "127.0.0.1:0".to_string(),
            allowed_hosts: None,
            max_connections: 4,
            statement_timeout_ms: 2_000,
            max_rows: 100,
        }
    }

    /// A fingerprint of everything a write could change: every table's row
    /// count and content hash, the sequence position, and the set of
    /// relations. Equal before and after means nothing was written.
    async fn fingerprint(&self) -> Result<String> {
        let row: (String,) = sqlx::query_as(
            "SELECT concat_ws(' | ',
                (SELECT string_agg(c.relname || ':' || c.relkind::text, ',' ORDER BY c.relname)
                   FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                  WHERE n.nspname IN ('public', 'forge')),
                (SELECT count(*) || '/' || md5(string_agg(u::text, '' ORDER BY id)) FROM forge.users u),
                (SELECT count(*) || '/' || md5(string_agg(r::text, '' ORDER BY id)) FROM forge.repos r),
                (SELECT count(*) || '/' || md5(string_agg(i::text, '' ORDER BY id)) FROM forge.issues i),
                (SELECT last_value::text FROM forge.issues_id_seq))",
        )
        .fetch_one(&self.admin)
        .await
        .wrap_err("fingerprint")?;
        Ok(row.0)
    }

    async fn drop_db(self) -> Result<()> {
        self.admin.close().await;
        let drop = format!("DROP DATABASE {} WITH (FORCE)", self.name);
        sqlx::raw_sql(sqlx::AssertSqlSafe(drop))
            .execute(&self.cluster)
            .await
            .wrap_err("DROP DATABASE")?;
        Ok(())
    }
}

/// A small forge: users own repos, repos have issues. Enough rows (5 000
/// issues) that the row cap is really exercised, plus a view, a sequence,
/// composite and partial indexes, and every type the renderer must handle.
const SEED: &str = r#"
CREATE SCHEMA forge;
CREATE TABLE forge.users (
    id         bigint PRIMARY KEY,
    login      text NOT NULL UNIQUE,
    full_name  text,
    created_at timestamptz NOT NULL
);
CREATE TABLE forge.repos (
    id         bigint PRIMARY KEY,
    owner_id   bigint NOT NULL REFERENCES forge.users (id),
    name       varchar(100) NOT NULL,
    stars      integer NOT NULL DEFAULT 0,
    size_mb    numeric(10, 2),
    topics     text[] NOT NULL DEFAULT '{}',
    settings   jsonb,
    UNIQUE (owner_id, name)
);
CREATE TABLE forge.issues (
    id        bigserial PRIMARY KEY,
    repo_id   bigint NOT NULL REFERENCES forge.repos (id),
    author_id bigint REFERENCES forge.users (id),
    title     text NOT NULL,
    closed    boolean NOT NULL DEFAULT false
);
CREATE INDEX issues_open_by_repo ON forge.issues (repo_id) WHERE NOT closed;
CREATE VIEW forge.open_issues AS SELECT * FROM forge.issues WHERE NOT closed;
CREATE TABLE public.audit_log (id bigint PRIMARY KEY, note text);
-- Volatile functions with side effects: a write that is *syntactically* a
-- SELECT, so only the READ ONLY transaction can stop it.
CREATE FUNCTION forge.log_note(n text) RETURNS bigint LANGUAGE sql VOLATILE AS
    'INSERT INTO public.audit_log VALUES ((SELECT coalesce(max(id), 0) + 1 FROM public.audit_log), n) RETURNING id';
CREATE FUNCTION forge.star(r bigint) RETURNS void LANGUAGE plpgsql VOLATILE AS
    $$BEGIN UPDATE forge.repos SET stars = stars + 1 WHERE id = r; END$$;
CREATE FUNCTION forge.nuke() RETURNS void LANGUAGE plpgsql VOLATILE AS
    $$BEGIN EXECUTE 'DROP TABLE forge.issues CASCADE'; END$$;

INSERT INTO forge.users VALUES
    (1, 'amadeus', 'Amadeus Mader',        '2024-01-15 09:30:00+00'),
    (2, 'renovate', NULL,                  '2024-02-01 00:00:00+00'),
    (3, 'zoë',      'Zoë Ünïcødé 🦀',      '2024-03-10 18:45:12.5+02');
INSERT INTO forge.repos VALUES
    (10, 1, 'homelab-mcp-servers', 42, 12.50, '{rust,mcp}', '{"default_branch": "main", "ci": {"provider": "github", "required": true}}'),
    (11, 1, 'pve-nixos-homelab',   7,  0.25,  '{nix}',       NULL),
    (12, 3, 'dotfiles',            0,  NULL,  '{}',          '{"archived": true}');
INSERT INTO forge.issues (repo_id, author_id, title, closed)
    SELECT 10 + (g % 3), 1 + (g % 3), 'issue #' || g, g % 4 = 0
    FROM generate_series(1, 5000) AS g;
ANALYZE;
"#;

async fn serve(config: &Config) -> Result<(TestServer, McpClient)> {
    let server = TestServer::start(pgmcp::router(config)?).await?;
    let client = server.connect().await?;
    Ok((server, client))
}

// ---- Contract ---------------------------------------------------------------

#[tokio::test]
async fn contract_tools_prompts_resources_and_server_info() {
    let db = TestDb::create().await.unwrap();
    let (_server, client) = serve(&db.config()).await.unwrap();

    insta::assert_json_snapshot!("contract", json!({
        "initialize": client.initialize_result(),
        "tools": client.list_tools().await.unwrap(),
        "prompts": client.list_prompts().await.unwrap(),
        "resources": client.list_resources().await.unwrap(),
        "prompt/schema_overview": client.get_prompt("schema_overview", json!({ "schema": "forge" })).await.unwrap(),
        "prompt/table_health": client.get_prompt("table_health", json!({ "table": "issues", "schema": "forge" })).await.unwrap(),
    }), { ".initialize.serverInfo.version" => "[version]" });

    let contents = client.read_resource("doc://pgmcp/guide").await.unwrap();
    let text = contents
        .first()
        .and_then(|c| c.get("text"))
        .and_then(Value::as_str);
    assert_eq!(text, Some(include_str!("../../README.md")));

    db.drop_db().await.unwrap();
}

// ---- Introspection ----------------------------------------------------------

/// "What is in this database?" answered the way an LLM would: schemas →
/// tables → one table's columns, indexes and foreign keys → stats.
#[tokio::test]
async fn introspection_walkthrough() {
    let db = TestDb::create().await.unwrap();
    let (_server, client) = serve(&db.config()).await.unwrap();

    let mut steps = serde_json::Map::new();
    for (label, tool, args) in [
        ("list_schemas", "list_schemas", json!({})),
        (
            "list_tables/forge",
            "list_tables",
            json!({ "schema": "forge" }),
        ),
        ("list_tables/all", "list_tables", json!({})),
        (
            "describe_table/repos",
            "describe_table",
            json!({ "schema": "forge", "table": "repos" }),
        ),
        (
            "describe_table/public_default",
            "describe_table",
            json!({ "table": "audit_log" }),
        ),
        (
            "list_indexes/issues",
            "list_indexes",
            json!({ "schema": "forge", "table": "issues" }),
        ),
        (
            "list_foreign_keys/issues",
            "list_foreign_keys",
            json!({ "schema": "forge", "table": "issues" }),
        ),
        (
            "describe_table/missing",
            "describe_table",
            json!({ "schema": "forge", "table": "nope" }),
        ),
    ] {
        steps.insert(
            label.into(),
            client.call_tool(tool, args).await.unwrap().json().unwrap(),
        );
    }
    insta::assert_json_snapshot!("introspection", Value::Object(steps));

    // Stats and size carry per-run numbers (on-disk bytes, scan counters,
    // the generated database name); assert their shape, not their values.
    let stats = client
        .call_tool("table_stats", json!({ "schema": "forge" }))
        .await
        .unwrap()
        .json()
        .unwrap();
    let mut tables: Vec<&str> = stats
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r.get("table_name").and_then(Value::as_str))
        .collect();
    tables.sort_unstable();
    assert_eq!(tables, ["issues", "repos", "users"]);
    let issues = stats
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r.get("table_name") == Some(&json!("issues")))
        .unwrap();
    // A number, not a value: `n_live_tup` comes from the cumulative stats
    // system, which backends flush asynchronously, so an exact count races.
    assert!(
        issues.get("estimated_rows").is_some_and(Value::is_i64),
        "estimated_rows must be an integer: {issues}"
    );

    let size = client
        .call_tool("database_size", json!({}))
        .await
        .unwrap()
        .json()
        .unwrap();
    let row = size.as_array().and_then(|a| a.first()).unwrap();
    assert_eq!(row.get("database"), Some(&json!(db.name)));
    assert!(row.get("size_bytes").and_then(Value::as_i64).unwrap() > 1_000_000);

    db.drop_db().await.unwrap();
}

/// Introspection arguments are bound, never interpolated: a table "name"
/// carrying SQL finds nothing and executes nothing.
#[tokio::test]
async fn introspection_arguments_cannot_inject_sql() {
    let db = TestDb::create().await.unwrap();
    let (_server, client) = serve(&db.config()).await.unwrap();
    let before = db.fingerprint().await.unwrap();

    for (tool, args) in [
        (
            "describe_table",
            json!({ "schema": "forge", "table": "users'; DROP TABLE forge.users; --" }),
        ),
        (
            "list_indexes",
            json!({ "schema": "forge' OR '1'='1", "table": "issues" }),
        ),
        (
            "list_tables",
            json!({ "schema": "%' ; DELETE FROM forge.issues; --" }),
        ),
    ] {
        let rows = client.call_tool(tool, args).await.unwrap().json().unwrap();
        assert_eq!(
            rows,
            json!([]),
            "{tool} must treat the argument as a literal"
        );
    }
    assert_eq!(db.fingerprint().await.unwrap(), before);
    db.drop_db().await.unwrap();
}

// ---- run_query: results -----------------------------------------------------

#[tokio::test]
async fn run_query_renders_every_type_faithfully() {
    let db = TestDb::create().await.unwrap();
    let (_server, client) = serve(&db.config()).await.unwrap();

    let rows = client
        .call_tool("run_query", json!({ "sql": "
            SELECT u.login, u.full_name, u.created_at, r.name, r.stars, r.size_mb, r.topics,
                   r.settings, r.settings -> 'ci' ->> 'provider' AS ci_provider,
                   (SELECT count(*) FROM forge.issues i WHERE i.repo_id = r.id AND NOT i.closed) AS open_issues
            FROM forge.users u LEFT JOIN forge.repos r ON r.owner_id = u.id
            ORDER BY u.id, r.id" }))
        .await
        .unwrap()
        .json()
        .unwrap();
    insta::assert_json_snapshot!("types", rows);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn run_query_row_cap_is_a_hard_ceiling() {
    let db = TestDb::create().await.unwrap();
    let (_server, client) = serve(&db.config()).await.unwrap();
    let sql = "SELECT id FROM forge.issues ORDER BY id";

    let count = |r: &ToolResult| r.json().unwrap().as_array().map(Vec::len).unwrap();
    // No limit: the server's cap (100), not all 5000 rows.
    assert_eq!(
        count(
            &client
                .call_tool("run_query", json!({ "sql": sql }))
                .await
                .unwrap()
        ),
        100
    );
    // Asking for more than the cap does not raise it.
    assert_eq!(
        count(
            &client
                .call_tool("run_query", json!({ "sql": sql, "limit": 10_000 }))
                .await
                .unwrap()
        ),
        100
    );
    // A smaller limit is honoured exactly.
    assert_eq!(
        count(
            &client
                .call_tool("run_query", json!({ "sql": sql, "limit": 7 }))
                .await
                .unwrap()
        ),
        7
    );
    // Degenerate limits clamp to one row instead of erroring.
    for limit in [0, -5, i64::MIN] {
        assert_eq!(
            count(
                &client
                    .call_tool("run_query", json!({ "sql": sql, "limit": limit }))
                    .await
                    .unwrap()
            ),
            1
        );
    }
    // The caller's own LIMIT still applies below the cap; an empty result is `[]`.
    assert_eq!(
        count(
            &client
                .call_tool("run_query", json!({ "sql": "SELECT 1 LIMIT 3" }))
                .await
                .unwrap()
        ),
        1
    );
    let empty = client
        .call_tool(
            "run_query",
            json!({ "sql": "SELECT id FROM forge.issues WHERE false" }),
        )
        .await
        .unwrap();
    assert_eq!(empty.json().unwrap(), json!([]));
    db.drop_db().await.unwrap();
}

// ---- run_query: the read-only guarantee -------------------------------------

/// Every way to write that a caller might try. Each must come back as an
/// error, and the fingerprint proves nothing changed. The error messages are
/// the snapshot artifact, so a change in *how* a write is refused is reviewed.
#[tokio::test]
async fn run_query_refuses_every_write_shape() {
    let db = TestDb::create().await.unwrap();
    let (_server, client) = serve(&db.config()).await.unwrap();
    let before = db.fingerprint().await.unwrap();

    let attempts = [
        (
            "insert",
            "INSERT INTO forge.users VALUES (99, 'mallory', NULL, now())",
        ),
        ("update", "UPDATE forge.repos SET stars = 0"),
        ("delete", "DELETE FROM forge.issues"),
        ("truncate", "TRUNCATE forge.issues CASCADE"),
        ("create_table", "CREATE TABLE forge.pwned (id int)"),
        ("drop_table", "DROP TABLE forge.issues CASCADE"),
        (
            "data_modifying_cte",
            "WITH d AS (DELETE FROM forge.issues RETURNING *) SELECT count(*) FROM d",
        ),
        ("nextval", "SELECT nextval('forge.issues_id_seq')"),
        ("setval", "SELECT setval('forge.issues_id_seq', 1)"),
        ("select_for_update", "SELECT * FROM forge.users FOR UPDATE"),
        (
            "flip_read_only",
            "SELECT set_config('transaction_read_only', 'off', false)",
        ),
        ("stacked_statement", "SELECT 1; DELETE FROM forge.issues"),
        (
            "paren_escape",
            "SELECT 1) AS _q; DELETE FROM forge.issues; SELECT * FROM (SELECT 1",
        ),
        ("commit_escape", "COMMIT; DELETE FROM forge.issues"),
        ("create_temp", "CREATE TEMP TABLE t AS SELECT 1"),
        // Valid SELECTs whose functions write: only READ ONLY stops these.
        ("function_insert", "SELECT forge.log_note('pwned')"),
        ("function_update", "SELECT forge.star(10) FROM forge.repos"),
        ("function_dynamic_ddl", "SELECT forge.nuke()"),
        ("large_object_create", "SELECT lo_create(0)"),
    ];

    let mut refused = serde_json::Map::new();
    for (label, sql) in attempts {
        let res = client
            .call_tool("run_query", json!({ "sql": sql }))
            .await
            .unwrap();
        let msg = res
            .error_message()
            .unwrap_or_else(|_| panic!("{label} was NOT refused"));
        refused.insert(
            label.into(),
            json!({ "sql": sql, "error": without_pg_source_lines(&msg) }),
        );
        // The session and the pool survive every refusal.
        client
            .call_tool("run_query", json!({ "sql": "SELECT 1 AS ok" }))
            .await
            .unwrap()
            .text()
            .unwrap();
    }

    assert_eq!(
        db.fingerprint().await.unwrap(),
        before,
        "a refused write left a trace"
    );
    insta::assert_json_snapshot!("refused_writes", Value::Object(refused));
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn statement_timeout_holds_even_when_the_query_tries_to_lift_it() {
    let db = TestDb::create().await.unwrap();
    let (_server, client) = serve(&db.config()).await.unwrap(); // 2 s timeout

    for sql in [
        "SELECT pg_sleep(30)",
        "SELECT set_config('statement_timeout', '0', true), pg_sleep(30)",
    ] {
        let started = Instant::now();
        let res = client
            .call_tool("run_query", json!({ "sql": sql }))
            .await
            .unwrap();
        let elapsed = started.elapsed();
        let msg = res.error_message().unwrap();
        assert!(msg.contains("statement timeout"), "{sql}: {msg}");
        assert!(elapsed < Duration::from_secs(8), "{sql} ran {elapsed:?}");
        client
            .call_tool("run_query", json!({ "sql": "SELECT 1" }))
            .await
            .unwrap()
            .text()
            .unwrap();
    }
    db.drop_db().await.unwrap();
}

/// Drop Postgres' own `at line N` (a line in *its* C source, e.g. `scan.l`),
/// which moves between Postgres releases and would churn the snapshot on every
/// upgrade without any change in behaviour.
fn without_pg_source_lines(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg;
    while let Some(i) = rest.find(" at line ") {
        out.push_str(rest.get(..i).unwrap_or_default());
        let tail = rest.get(i + " at line ".len()..).unwrap_or_default();
        rest = tail.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    out.push_str(rest);
    out
}

// ---- Concurrency and availability -------------------------------------------

/// Eight sessions × five calls against a pool of four connections, all at
/// once, including slow queries holding connections. Nothing may fail or
/// deadlock, and every answer must be the right one.
#[tokio::test]
async fn concurrent_sessions_share_the_pool_without_errors() {
    let db = TestDb::create().await.unwrap();
    let server = TestServer::start(pgmcp::router(&db.config()).unwrap())
        .await
        .unwrap();

    let mut tasks = Vec::new();
    for session in 0..8_i64 {
        let url = server.mcp_url();
        tasks.push(tokio::spawn(async move {
            let client = McpClient::connect(&url).await?;
            for call in 0..5_i64 {
                let n = session * 10 + call;
                let rows = client
                    .call_tool(
                        "run_query",
                        json!({ "sql": format!("SELECT {n} AS n, pg_sleep(0.05)::text AS s") }),
                    )
                    .await?
                    .json()?;
                let got = rows.pointer("/0/n").and_then(Value::as_i64);
                if got != Some(n) {
                    return Err(eyre!(
                        "session {session} call {call}: expected {n}, got {rows}"
                    ));
                }
            }
            Ok::<(), color_eyre::Report>(())
        }));
    }
    for t in tasks {
        t.await.unwrap().unwrap();
    }
    db.drop_db().await.unwrap();
}

/// The pool connects lazily, so a server whose database is down must still
/// start and list its tools, and each call must fail promptly with the cause
/// rather than hang until the client gives up.
#[tokio::test]
async fn unreachable_database_fails_calls_promptly_but_the_server_runs() {
    let cfg = Config {
        database_url: "postgres://postgres@localhost/nope?host=/nonexistent/socket-dir".to_string(),
        bind: "127.0.0.1:0".to_string(),
        allowed_hosts: None,
        max_connections: 2,
        statement_timeout_ms: 2_000,
        max_rows: 100,
    };
    let (_server, client) = serve(&cfg).await.unwrap();
    assert!(!client.list_tools().await.unwrap().is_empty());

    let started = Instant::now();
    let res = client.call_tool("list_schemas", json!({})).await.unwrap();
    let msg = res.error_message().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
    assert!(
        msg.contains("transaction") || msg.contains("connect"),
        "{msg}"
    );
}
