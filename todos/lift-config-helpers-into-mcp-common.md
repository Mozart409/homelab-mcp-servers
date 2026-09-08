# Lift `normalize_base_url` / `parse_allowed_hosts` into `mcp-common`

Three REST servers now carry byte-identical copies of the same two config
helpers:

- `crates/prometheus-mcp/prommcp/src/config.rs`
- `crates/loki-mcp/lokimcp/src/config.rs`
- `crates/alertmanager-mcp/alertmanagermcp/src/config.rs`

Along with their tests, that is roughly 60 duplicated lines per crate, and the
`parse_allowed_hosts` copy carries a safety property worth stating once rather
than three times: an allow-list that parses to empty must collapse to `None`,
because `Some(vec![])` makes rmcp reject *every* inbound `Host` header and
produces a server that silently accepts no connections.

## Why it was deferred

Noticed while adding `alertmanager-mcp`, which made the third copy. Lifting them
would have meant touching two working servers in a branch that was already
introducing a new crate and rewriting Hard rule §1 — three unrelated reasons for
one review to reject the diff.

## Shape

`normalize_base_url` differs per server only in its default port (9090, 3100,
9093), so it wants a port parameter:

```rust
pub fn normalize_base_url(host: &str, default_port: u16) -> String
```

`parse_allowed_hosts` is identical everywhere and moves as-is.

Move the unit tests with them; keep one per-crate test asserting that crate's
default port, since that is the part the shared helper cannot cover.

## Watch out for

`hamcp` and `pbsmcp` parse their base URLs differently (`hamcp` uses `Url::parse`
and stores a `Url`, not a `String`). Don't fold those into the shared helper in
the same pass — they are a separate decision.
