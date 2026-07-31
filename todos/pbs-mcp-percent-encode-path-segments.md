# pbs-mcp: percent-encode user-supplied path segments

**Severity:** bug — silent breakage, tools are not composable
**Crate:** `crates/pbs-mcp/pbsmcp`
**Found:** 2026-07-28, while chasing a stuck PBS backup task from the homelab agent

## Symptom

`pbs_task_log` cannot consume a UPID that `pbs_list_tasks` just returned. The
list-then-inspect workflow — the obvious way to use these tools — fails on any
task whose UPID contains escaped bytes.

`pbs_list_tasks` returns UPIDs verbatim from PBS, and PBS escapes non-alphanumeric
characters in the worker-id field as `\xNN`:

```
UPID:pbs:000002CE:00003BCB:0000006E:6A68082F:backup:r2\x2dstore\x3act-104:root@pam:
                                                        ^^^^      ^^^^
                                                        "-"       ":"
```

Feeding that straight back into `pbs_task_log` yields:

```
PBS API .../tasks/UPID:...:backup:r2\x2dstore\x3act-104:root@pam:/log
returned 404 Not Found:
Path '/api2/json/nodes/localhost/tasks/UPID:...:backup:r2/x2dstore/x3act-104:root@pam:/log' not found.
```

Note PBS's own error message echoes the mangled path: the backslashes were
interpreted as **path separators**, so one segment became three and the route
no longer matched.

Manually substituting `\` → `%5C` in the UPID makes the call succeed, which
confirms the diagnosis.

## Root cause

`crates/pbs-mcp/pbsmcp/src/client.rs:59`

```rust
let url = format!("{}/api2/json{}", self.base_url, path);
```

The path is assembled by raw string interpolation in the callers and handed to
`reqwest` as a complete URL string. Nothing percent-encodes the interpolated
values, so any reserved character in user- or PBS-supplied data (`\`, `/`, `?`,
`#`, `%`, space) changes the URL's structure instead of being carried as data.

`reqwest::Client::get(&str)` parses the string as a whole URL — it does not and
cannot know which parts were meant to be a single segment.

## Affected call sites

All of these interpolate a caller-controlled value into a path segment
(`crates/pbs-mcp/pbsmcp/src/server.rs`):

| Line | Path | Interpolated |
|------|------|--------------|
| ~200 | `/nodes/{node}/tasks/{upid}/status` | `upid` — **confirmed broken** |
| ~211 | `/nodes/{node}/tasks/{upid}/log` | `upid` — **confirmed broken** |
| ~121 | `/admin/datastore/{store}/status` | `store` |
| — | `/admin/datastore/{store}/groups` | `store` |
| — | `/admin/datastore/{store}/snapshots` | `store` |
| — | `/nodes/{node}/status` | `node` |
| — | `/nodes/{node}/tasks` | `node` |

`upid` is the confirmed failure because PBS *generates* the escapes. The `store`
and `node` cases are latent: they only break on a datastore name containing a
reserved character, but they're the same defect and should be fixed together
rather than left as a trap.

## Suggested fix

Encode each segment at the point of interpolation, not inside `get()` — `get()`
receives an already-joined path and can no longer tell segment boundaries from
structural slashes.

Add a helper (e.g. in `client.rs`, re-exported for `server.rs`):

```rust
/// Percent-encode a single URL path segment.
///
/// PBS embeds `\xNN` escapes in UPIDs and permits reserved characters in
/// datastore names; interpolating those raw would change the URL's structure.
fn seg(s: &str) -> impl std::fmt::Display + '_ {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC)
}
```

Then at each call site:

```rust
self.call(&format!("/nodes/{}/tasks/{}/log", seg(node), seg(upid)), &q)
```

`percent_encoding` is already in the dependency tree via `reqwest`/`url`, but
add it as a direct dependency rather than relying on a transitive one.

### On the encoding set

`NON_ALPHANUMERIC` is deliberately aggressive. A narrower set risks missing a
character PBS decides to emit later. The cost is only URL verbosity —
`root@pam:` becomes `root%40pam%3A`, which PBS decodes correctly. Do **not** use
a set that leaves `\` or `/` unencoded; those are the exact characters that
caused this.

### Alternative worth considering

Switch `get()` to take `&[&str]` segments and build the URL with
`url::Url::path_segments_mut()`, which encodes correctly by construction and
makes the bug class unrepresentable. Larger refactor, but it removes the
need for every future call site to remember `seg()`.

## Verification

Regression test — round-trip a realistic UPID through both tools:

1. `pbs_list_tasks` → take any UPID containing `\x`
2. `pbs_task_log` with that exact string → expect `200`, not `404`
3. `pbs_task_status` with the same → expect `200`

Unit test the helper directly against the real-world value:

```rust
assert_eq!(
    seg(r"backup:r2\x2dstore\x3act-104").to_string(),
    "backup%3Ar2%5Cx2dstore%5Cx3act%2D104"
);
```

Also add a case for a datastore name with a `-`/`:` to lock in the `store`
path fix.

## Notes

- Check whether the sibling crates (`pgmcp`, `prommcp`, `lokimcp`, `hamcp`)
  build URLs the same way. Loki label values and Prometheus series selectors
  are very likely to contain reserved characters, so this may be a shared
  defect rather than a pbs-mcp one.
