# pbs-mcp: `task_log` cannot reach the end of a log

**Severity:** enhancement — blocks the primary use case for long-running tasks
**Crate:** `crates/pbs-mcp/pbsmcp`
**Found:** 2026-07-28, while inspecting an 8-hour PBS backup from the homelab agent

## Symptom

`pbs_task_log` can only return lines from the **start** of a log. For a task
that has been running for hours, the interesting lines are at the end — is it
progressing, stalled, or throwing errors? — and there is no way to reach them.

Real case: a `backup r2-store:ct/104` task ran for 8+ hours. The question that
mattered was "is it wedged or just slow?", answerable only from the tail. The
first 100 lines showed a healthy startup and told us nothing. Falling back to
raw `curl` against the PBS API answered it in one call:

```
11:46:34: upload_chunk done: 1129568 bytes, 6616c58b...
11:46:36: upload_chunk done: 2527890 bytes, 4850fe6b...
11:46:38: upload_chunk done: 1400782 bytes, 6945e789...
```

→ ~1.26 MB/s, progressing, upload-bandwidth-bound. Not stalled.

Having to drop to `curl` to answer that defeats the point of the tool.

## Root cause

Two independent gaps.

### 1. `start` is not exposed

`crates/pbs-mcp/pbsmcp/src/server.rs` (~line 97):

```rust
struct TaskLogParams {
    /// Task UPID, as returned by `list_tasks`.
    upid: String,
    /// Maximum number of log lines to return (default: 100).
    #[serde(default)]
    limit: Option<u64>,
}
```

PBS's `GET /nodes/{node}/tasks/{upid}/log` accepts both `start` and `limit`.
Only `limit` is surfaced, and PBS applies it from line 1 — so `limit` is
"first N", never "last N".

### 2. `total` is discarded before the caller sees it

`crates/pbs-mcp/pbsmcp/src/client.rs:82`:

```rust
// PBS wraps payloads as `{ "data": ... }`; unwrap when present.
Ok(json.get_mut("data").map(Value::take).unwrap_or(json))
```

PBS returns the log line count as `total` **in the envelope, as a sibling of
`data`**. Unwrapping to `data` throws it away. So even if `start` were exposed,
a caller could not compute `start = total - N` without already knowing `total` —
there is no way to discover it through the tool.

These compound: the ergonomic fix needs both.

## Suggested fix

### Expose `start`

```rust
struct TaskLogParams {
    /// Task UPID, as returned by `list_tasks`.
    upid: String,
    /// First line to return (1-based). Omit to start at the beginning.
    #[serde(default)]
    start: Option<u64>,
    /// Maximum number of log lines to return (default: 100).
    #[serde(default)]
    limit: Option<u64>,
    /// Return the LAST N lines instead of the first N. Overrides `start`.
    #[serde(default)]
    tail: Option<u64>,
}
```

### Implement `tail`

`tail` is what callers actually want, and it should be one round trip. PBS
returns `total` on every log request, so:

1. Request `start=0&limit=1` to read `total` from the envelope (cheap).
2. Re-request with `start = total.saturating_sub(tail)` and `limit = tail`.

Two upstream calls, one tool call. Acceptable. If a single call is preferred,
`limit=0` returns the whole log and the server can slice locally — simpler, but
unbounded memory on a large log, so prefer the two-step.

Guard: `tail` and `start` are mutually exclusive; document that `tail` wins, or
reject the combination with a clear error rather than silently picking one.

### Surface `total`

Preserve the envelope metadata so callers can page. Options, in order of
preference:

1. **Wrap `task_log`'s own response**: return `{"total": N, "start": S, "lines": [...]}`.
   Keeps the generic `get()` unwrapping untouched, fixes the tool that needs it.
2. Add a `get_envelope()` to `client.rs` returning the full JSON, used only by
   paging-aware tools.
3. Stop unwrapping `data` globally — **rejected**, it would change the shape of
   every other tool's output for no benefit.

Go with (1).

## Also update the tool description

Current: *"Read the log output of a task by its UPID."*

It should state that output is from the start by default, that `tail` exists for
running tasks, and that `total` is returned for paging. Agents pick tools from
these descriptions; if `tail` isn't mentioned, it won't get used and the tool
will keep being bypassed with `curl`.

## Verification

- Long log, `tail: 12` → returns exactly the last 12 lines; matches
  `curl '.../log?start=0&limit=0' | jq '.data[-12:]'`.
- `tail` larger than `total` → returns the whole log, no error, no underflow
  (hence `saturating_sub`).
- Running task → two consecutive `tail` calls show a growing `total` and
  different last lines.
- Finished task → `total` is stable across calls.
- `start` + `limit` still page correctly through the middle of a log.

## Dependency

Depends on [pbs-mcp-percent-encode-path-segments](./pbs-mcp-percent-encode-path-segments.md).
`task_log` is unreachable for any real UPID until the encoding bug is fixed, so
none of this is testable end-to-end beforehand. **Fix the encoding bug first.**
