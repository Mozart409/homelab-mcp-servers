#!/usr/bin/env bash
# A throwaway PostgreSQL cluster for the pgmcp test suite, tuned for speed.
#
#   scripts/test-pg.sh run -- <command...>   start, export the URL, run, stop
#   eval "$(scripts/test-pg.sh start)"       start and print `export` lines
#   scripts/test-pg.sh stop                  stop the cluster `start` made
#
# `just test` uses `run`. The crane `checks.test` derivation uses start/stop
# around its checkPhase, because crane's test command is a shell function that
# a wrapper script cannot call.
#
# The command sees PGMCP_TEST_DATABASE_URL, a superuser URL for the `postgres`
# database. Each pgmcp test creates its own database from it, so tests run in
# parallel without sharing state.
#
# WHY IT IS FAST:
# - The data directory lives on tmpfs ($XDG_RUNTIME_DIR, else $TMPDIR). Nothing
#   touches the disk.
# - Durability is switched off entirely: fsync, synchronous_commit,
#   full_page_writes, and WAL down to `minimal`. A crash loses the cluster,
#   which is thrown away at the end of the run anyway.
# - Autovacuum, JIT and checkpoints are out of the way. Every test database is
#   created and dropped within seconds.
# - Unix socket only (`listen_addresses = ''`): no TCP port to collide with a
#   developer's own Postgres, or with a second concurrent run.
# - `max_connections` is high because every test runs its own pool in parallel
#   with the rest; running out of slots would surface as flaky timeouts.
#
# WHY NOT compose.yaml's postgres: that one is the stack's database, reached
# with real credentials via sops. Tests must not need secrets or a running
# stack, and must not be able to damage either.
set -euo pipefail

state_file() { echo "${TEST_PG_STATE:-${TMPDIR:-/tmp}/pgmcp-test-pg.dir}"; }

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "error: '$1' not found; PostgreSQL must be on PATH (it is in the nix dev shell: \`nix develop\`)" >&2
        exit 1
    }
}

start() {
    need initdb
    need pg_ctl
    local root="${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}"
    [ -w "$root" ] || root="${TMPDIR:-/tmp}"
    local dir
    dir="$(mktemp -d "$root/pgmcp-test.XXXXXX")"

    initdb -D "$dir/data" -U postgres -A trust --no-sync --no-instructions \
        --no-locale -E UTF8 >"$dir/initdb.log" 2>&1 || {
        cat "$dir/initdb.log" >&2
        exit 1
    }

    cat >>"$dir/data/postgresql.conf" <<EOF
listen_addresses = ''
unix_socket_directories = '$dir'
max_connections = 300
shared_buffers = 128MB
fsync = off
synchronous_commit = off
full_page_writes = off
wal_level = minimal
max_wal_senders = 0
max_wal_size = 4GB
checkpoint_timeout = 1d
autovacuum = off
jit = off
dynamic_shared_memory_type = mmap
log_min_messages = warning
EOF

    pg_ctl -D "$dir/data" -l "$dir/postgres.log" -w -t 30 start >/dev/null || {
        cat "$dir/postgres.log" >&2
        exit 1
    }

    echo "$dir" >"$(state_file)"
    echo "export PGMCP_TEST_DATABASE_URL='postgres://postgres@localhost/postgres?host=$dir'"
}

stop() {
    local sf dir
    sf="$(state_file)"
    [ -f "$sf" ] || return 0
    dir="$(cat "$sf")"
    pg_ctl -D "$dir/data" -m immediate stop >/dev/null 2>&1 || true
    rm -rf "$dir" "$sf"
}

case "${1:-}" in
start) start ;;
stop) stop ;;
run)
    shift
    [ "${1:-}" = "--" ] && shift
    [ $# -gt 0 ] || { echo "usage: $0 run -- <command...>" >&2; exit 2; }
    # A per-invocation state file, so two concurrent runs never stop each
    # other's cluster.
    TEST_PG_STATE="$(mktemp "${TMPDIR:-/tmp}/pgmcp-test-pg.XXXXXX")"
    export TEST_PG_STATE
    trap stop EXIT INT TERM
    eval "$(start)"
    "$@"
    ;;
*)
    echo "usage: $0 {run -- <command...>|start|stop}" >&2
    exit 2
    ;;
esac
