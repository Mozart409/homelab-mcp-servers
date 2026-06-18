# `server-mcp` template

Copy-source for a new homelab MCP server. Two crates: a library (`mymcp`) and a
thin binary (`mymcp-server`). Lives outside `crates/` on purpose so the workspace
glob (`crates/*/*`) never compiles the placeholder.

## Adding a new server

Pick a short name (e.g. `forgejo` → abbreviation `fgmcp`). From the repo root:

```sh
NAME=fgmcp                    # your server's short name
cp -r templates/server-mcp crates/forgejo-mcp
cd crates/forgejo-mcp

# rename the crate dirs
mv mymcp "$NAME"
mv mymcp-server "$NAME-server"

# replace the placeholder name everywhere
grep -rl mymcp . | xargs sed -i "s/mymcp/$NAME/g"
```

Then:

1. Update each crate's `description` (drop the `TODO:` placeholders).
2. Add the deps your server needs (`sqlx`, `reqwest`, `axum`, …) by inheriting
   from the workspace: `<dep>.workspace = true`. If the crate isn't in the root
   `[workspace.dependencies]` yet, add it there first.
3. `cargo check -p "$NAME-server"` to confirm it builds.

Versions/edition are inherited from `[workspace.package]`, so the copied crates
build as soon as they land under `crates/`.
