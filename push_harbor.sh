#!/usr/bin/env bash
#
# Build every MCP server image and push it to the homelab Harbor registry.
#
# Safety checks before anything is built or pushed:
#   * the git working tree must be clean
#   * HEAD must point at an annotated/lightweight git tag
#   * that tag must match the workspace version in Cargo.toml
#
# Usage: ./push_harbor.sh
set -euo pipefail

REGISTRY="homelab-harbor.dropbear-butterfly.ts.net/mcp-servers"
SERVERS=(pbsmcp-server pgmcp-server prommcp-server lokimcp-server hamcp-server)

# Always operate from the repo root regardless of where we're invoked from.
cd "$(git rev-parse --show-toplevel)"

die() {
    echo "error: $*" >&2
    exit 1
}

# --- the working tree must be clean ---------------------------------------
if [[ -n "$(git status --porcelain)" ]]; then
    die "git working tree is not clean — commit or stash your changes first."
fi

# --- HEAD must be exactly tagged ------------------------------------------
if ! TAG="$(git describe --exact-match --tags HEAD 2>/dev/null)"; then
    die "HEAD is not tagged — tag the release commit before pushing."
fi

# --- the tag must match the workspace version -----------------------------
VERSION="$(awk '
    /^\[/                { section = $0 }
    section == "[workspace.package]" && /^[[:space:]]*version[[:space:]]*=/ {
        gsub(/.*=[[:space:]]*"?/, ""); gsub(/".*/, ""); print; exit
    }
' Cargo.toml)"

[[ -n "$VERSION" ]] || die "could not read version from [workspace.package] in Cargo.toml."

TAG_VERSION="${TAG#v}" # tolerate both "v0.1.0" and "0.1.0"
if [[ "$TAG_VERSION" != "$VERSION" ]]; then
    die "git tag ($TAG -> $TAG_VERSION) does not match Cargo version ($VERSION)."
fi

echo "==> releasing version $VERSION (tag $TAG) to $REGISTRY"

# --- build and push each server -------------------------------------------
for bin in "${SERVERS[@]}"; do
    local_tag="${bin}:${VERSION}"
    remote="${REGISTRY}/${bin}:${VERSION}"
    remote_latest="${REGISTRY}/${bin}:latest"

    echo "==> building ${local_tag}"
    podman build --build-arg "BIN=${bin}" -t "${local_tag}" -f Containerfile .

    echo "==> pushing ${remote}"
    podman push "${local_tag}" "${remote}"

    echo "==> pushing ${remote_latest}"
    podman push "${local_tag}" "${remote_latest}"
done

echo "==> done — pushed ${#SERVERS[@]} image(s) at version $VERSION"
