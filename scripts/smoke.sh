#!/usr/bin/env bash
#
# Smoke-test a built server image: does the container come up, does the
# distroless self-healthcheck work, and does the MCP endpoint answer?
#
# WHY THIS IS NOT A GIT HOOK: it needs a container build (minutes, not seconds),
# and `--live` needs network access to the real backend. A pre-push hook that
# fails when you are off the tailnet is a hook people learn to `--no-verify`
# past, which would disable the clippy and `cog verify` checks that do belong
# there. Run this deliberately when adding or deploying a server instead.
#
# Usage: scripts/smoke.sh <bin> [tag] [--live]
#
#   scripts/smoke.sh alertmanagermcp-server
#   scripts/smoke.sh alertmanagermcp-server dev --live
#
# Without --live the container is started with a placeholder host, so the checks
# are entirely local: nothing outside the machine is contacted. With --live the
# image is given the real env (ENV_FILE, decrypted from .sops.env by `just
# smoke-live`) and one tool is called, which exercises TLS and
# the backend credentials.
set -uo pipefail

BIN=${1:-}
TAG=${2:-dev}
LIVE=false
for arg in "$@"; do [ "$arg" = "--live" ] && LIVE=true; done

if [ -z "$BIN" ]; then
    echo "usage: scripts/smoke.sh <bin> [tag] [--live]" >&2
    exit 2
fi

# Env prefix and in-container port per server. Kept in step with `knownServers`
# in flake.nix and the port list in AGENTS.md — a new server needs a line here,
# the same way it needs one in `just image-all`.
case "$BIN" in
    pbsmcp-server)           PREFIX=PBS;          PORT=8080 ;;
    pgmcp-server)            PREFIX=PG;           PORT=8081 ;;
    prommcp-server)          PREFIX=PROM;         PORT=8082 ;;
    lokimcp-server)          PREFIX=LOKI;         PORT=8083 ;;
    hamcp-server)            PREFIX=HA;           PORT=8084 ;;
    wpmcp-server)            PREFIX=WP;           PORT=8085 ;;
    alertmanagermcp-server)  PREFIX=ALERTMANAGER; PORT=8086 ;;
    tempomcp-server)         PREFIX=TEMPO;        PORT=8092 ;;
    *) echo "unknown server '$BIN' — add it to the case in scripts/smoke.sh" >&2; exit 2 ;;
esac

IMG="$BIN:$TAG"
NAME="${BIN}-smoke"
HOSTPORT=$((PORT + 10000))
FAILED=0

note() { printf '  %s\n' "$*"; }
pass() { printf 'PASS  %s\n' "$*"; }
fail() { printf 'FAIL  %s\n' "$*"; FAILED=1; }

cleanup() { podman rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT
cleanup

if ! podman image exists "$IMG"; then
    echo "image $IMG not found — build it first (just image $BIN)" >&2
    exit 2
fi

echo "== smoke: $IMG (prefix ${PREFIX}_, port $PORT, live=$LIVE) =="

# ---- 1. start ---------------------------------------------------------------
RUN_ARGS=(-d --name "$NAME" -e "${PREFIX}_BIND=0.0.0.0:$PORT" -e RUST_LOG=info
          -p "$HOSTPORT:$PORT")
if [ "$LIVE" = true ]; then
    # ENV_FILE is a sops-decrypted temp file when run via `just smoke-live`;
    # -r rather than -f so a FIFO or tmpfs file is accepted as well.
    ENV_FILE=${ENV_FILE:-.env}
    [ -r "$ENV_FILE" ] || { echo "$ENV_FILE is required for --live (set ENV_FILE or run via just smoke-live)" >&2; exit 2; }
    RUN_ARGS+=(--env-file "$ENV_FILE")
else
    # Enough config to boot without reaching anything real.
    RUN_ARGS+=(-e "${PREFIX}_HOST=http://127.0.0.1:1"
               -e "${PREFIX}_TOKEN=smoke"
               -e "${PREFIX}_DATABASE_URL=postgres://smoke@127.0.0.1:1/smoke"
               -e "${PREFIX}_API_KEY=smoke")
fi

if ! podman run "${RUN_ARGS[@]}" "$IMG" >/dev/null 2>&1; then
    fail "container did not start"
    exit 1
fi

for _ in $(seq 1 60); do
    curl -s -m 2 -o /dev/null "http://127.0.0.1:$HOSTPORT/_healthcheck" && break
    sleep 0.5
done

# ---- 2. the distroless self-probe ------------------------------------------
# The image has no shell and no curl, so the container healthcheck is the binary
# probing itself. If this regresses, every deployment reports unhealthy.
if podman exec "$NAME" /usr/local/bin/server --healthcheck >/dev/null 2>&1; then
    pass "--healthcheck self-probe"
else
    fail "--healthcheck self-probe returned non-zero"
fi

# ---- 3. MCP endpoint --------------------------------------------------------
U="http://127.0.0.1:$HOSTPORT/mcp"
H=(-H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream')
SID=$(curl -s -D- -o /dev/null "${H[@]}" -X POST "$U" -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' 2>/dev/null \
    | tr -d '\r' | awk -F': ' 'tolower($1)=="mcp-session-id"{print $2}')

if [ -n "$SID" ]; then
    pass "MCP initialize (session $SID)"
    curl -s "${H[@]}" -H "Mcp-Session-Id: $SID" -X POST "$U" \
        -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' >/dev/null 2>&1

    TOOLS=$(curl -s "${H[@]}" -H "Mcp-Session-Id: $SID" -X POST "$U" \
        -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' 2>/dev/null \
        | sed 's/^data: //' | grep -o '"name":"[a-z_]*"' | sort -u)
    COUNT=$(printf '%s' "$TOOLS" | grep -c '"name"')

    if [ "$COUNT" -gt 0 ]; then
        pass "tools/list returned $COUNT tools"
        printf '%s\n' "$TOOLS" | sed 's/"name":"//;s/"//' | sed 's/^/      /'
    else
        fail "tools/list returned no tools"
    fi
else
    fail "MCP initialize did not return a session id"
fi

# ---- 4. one live tool call (opt-in) ----------------------------------------
# Only under --live: this is the check that proves TLS trust and credentials
# inside the container, which is where a homelab CA typically bites — the
# distroless base carries public roots only.
if [ "$LIVE" = true ] && [ -n "$SID" ]; then
    FIRST=$(printf '%s\n' "$TOOLS" | sed 's/"name":"//;s/"//' | head -1)
    RESP=$(curl -s "${H[@]}" -H "Mcp-Session-Id: $SID" -X POST "$U" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"$FIRST\",\"arguments\":{}}}" 2>/dev/null | sed 's/^data: //')
    if echo "$RESP" | grep -q '"isError":true\|"error"'; then
        if echo "$RESP" | grep -qi 'certificate\|UnknownIssuer\|tls'; then
            fail "live call '$FIRST': TLS trust — is the homelab CA in the image?"
        else
            fail "live call '$FIRST' returned an error"
        fi
        note "$(echo "$RESP" | head -c 300)"
    else
        pass "live call '$FIRST' reached the backend"
    fi
fi

# ---- 5. still up ------------------------------------------------------------
STATUS=$(podman ps --filter "name=$NAME" --format '{{.Status}}' 2>/dev/null)
if [ -n "$STATUS" ]; then
    pass "container still running ($STATUS)"
else
    fail "container exited during the run"
    note "$(podman logs "$NAME" 2>&1 | tail -5)"
fi

echo
[ "$FAILED" = 0 ] && echo "smoke: OK" || echo "smoke: FAILED"
exit "$FAILED"
