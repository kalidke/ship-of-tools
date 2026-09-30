#!/usr/bin/env bash
# test-agent-join.sh — hermetic suite for `comm-join.sh`'s `agent.join`
# declaration (ADR 0046 decision 1): a session inside a daemon-spawned
# workspace (`$SOT_WORKSPACE_ID` set) declares its handle to the daemon
# that pinned its env, over the typed owner endpoint (`$SOT_SOCKET`) —
# never the relay. No bats dependency, no real daemon: a STUB unix-socket
# listener (same technique as test-sot-fe-version.sh) answers `hello`/
# `agent.join` with canned replies and logs every request this file
# inspects. Never touches a real ~/.sot-comm (a temp $SOT_COMM_HOME).
#
# Usage: comm/core/tests/test-agent-join.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
JOIN="$SCRIPTS_DIR/comm-join.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-agent-join-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi

export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
mkdir -p "$SOT_COMM_HOME"
# Hermetic host: comm-context.sh reads $SOT_COMM_TEST_HOST exactly as
# every other comm test does (manager review: no on-disk namespace or
# self-file-key changes this sprint, ADR 0046 decision 1's S1 ruling —
# comm-context.sh's own HOST resolution, and therefore this test's pin,
# stays untouched by the declared-identity lane).
export SOT_COMM_TEST_HOST="test-host"
# Hermetic row binding (2026-09-28). A session that runs this suite has
# $SOT_WORKSPACE_ID and $SOT_SOCKET pinned in its own environment, and every
# case here inherits them. The "no SOT_WORKSPACE_ID pinned" case below was
# therefore never running without one: it declared a handle into whichever
# live row happened to be running the suite, and passed anyway because the
# stub daemon it checked was not the daemon that received it. Each case pins
# what it needs and nothing is ambient — the self file included, since a case
# that supplies one is supplying the identity itself.
unset SOT_WORKSPACE_ID SOT_SOCKET SOT_COMM_SELF_FILE

STUB_NC_PID=""
STUB_WATCHER_PID=""
cleanup() {
    [ -n "$STUB_WATCHER_PID" ] && kill "$STUB_WATCHER_PID" >/dev/null 2>&1
    [ -n "$STUB_NC_PID" ] && kill "$STUB_NC_PID" >/dev/null 2>&1
    exec 3>&- 2>/dev/null || true
    rm -rf "${WORK:?}"
}
trap cleanup EXIT

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2" rc
    "$fn"
    rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "PASS: $desc"; PASS=$((PASS + 1))
    else
        echo "FAIL: $desc"; FAIL=$((FAIL + 1))
    fi
}

contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# --- the stub daemon -------------------------------------------------------
#
# Answers `hello` unconditionally ok (comm-join.sh's agent.join call goes
# through sot_oneshot_request, which sends a hello first on every fresh
# connection) and `agent.join` with `{ok:true}`. Mirrors
# test-sot-fe-version.sh's own stub daemon shape exactly — see that
# file's header for why a persistent `nc -klU` + FIFO + `tail -F` watcher
# is used instead of a one-shot `nc`.
SOCK=""
REQLOG=""
STUBN=0
start_stub_daemon() {
    STUBN=$((STUBN + 1))
    SOCK="$WORK/stub-$STUBN.sock"
    local fifo="$WORK/resp-$STUBN.fifo"
    REQLOG="$WORK/req-$STUBN.log"
    mkfifo "$fifo"
    : > "$REQLOG"

    exec 3<>"$fifo"
    nc -klU "$SOCK" < "$fifo" >> "$REQLOG" &
    STUB_NC_PID=$!

    ( tail -n +1 -F "$REQLOG" 2>/dev/null | while IFS= read -r line; do
        op="$(printf '%s' "$line" | jq -r '.op // empty' 2>/dev/null)"
        case "$op" in
            hello)
                printf '%s\n' '{"v":1,"id":1,"kind":"res","op":"hello","payload":{"session_id":"s1","revision":0,"snapshot_pending":false}}' >&3
                ;;
            agent.join)
                printf '%s\n' '{"v":1,"id":1,"kind":"res","op":"agent.join","payload":{"ok":true}}' >&3
                ;;
        esac
    done ) &
    STUB_WATCHER_PID=$!

    local deadline=$((SECONDS + 5))
    while [ ! -S "$SOCK" ]; do
        [ "$SECONDS" -lt "$deadline" ] || { echo "stub daemon socket never appeared: $SOCK" >&2; break; }
        sleep 0.05
    done
}

stop_stub_daemon() {
    [ -n "$STUB_WATCHER_PID" ] && pkill -TERM -P "$STUB_WATCHER_PID" >/dev/null 2>&1
    [ -n "$STUB_WATCHER_PID" ] && kill "$STUB_WATCHER_PID" >/dev/null 2>&1
    [ -n "$STUB_NC_PID" ] && kill "$STUB_NC_PID" >/dev/null 2>&1
    wait "$STUB_WATCHER_PID" 2>/dev/null
    wait "$STUB_NC_PID" 2>/dev/null
    exec 3>&- 2>/dev/null || true
    STUB_NC_PID=""
    STUB_WATCHER_PID=""
}

# join_in ROOT [ARGS...] — run comm-join.sh in ROOT with a fresh self-file
# slot (never the shared nopane one — each case gets its own identity
# slot so cases can't interfere), capturing stdout/stderr/exit. Any
# SOT_WORKSPACE_ID/SOT_SOCKET the CALLER prefixed onto this invocation
# (`VAR=val join_in ...`) reach comm-join.sh unchanged — bash exports a
# function-call prefix assignment into every command the function runs.
JOIN_OUT=""; JOIN_ERR=""; JOIN_RC=0
NEXTSELF=0
join_in() {
    local root="$1"; shift
    NEXTSELF=$((NEXTSELF + 1))
    # A real session's self file is the one comm-context.sh names for its row,
    # "<host>__<workspace_id>.txt", and that basename is where the declaration
    # reads the row id from. So a case that pins a row id gets an identity that
    # names it, and a case that wants some OTHER identity pins
    # $SOT_COMM_SELF_FILE itself — the production variable, never a test-only
    # knob beside it.
    local self="${SOT_COMM_SELF_FILE:-}"
    if [ -z "$self" ]; then
        if [ -n "${SOT_WORKSPACE_ID:-}" ]; then
            self="$WORK/${SOT_COMM_TEST_HOST}__${SOT_WORKSPACE_ID}.txt"
        else
            self="$WORK/self-$NEXTSELF.txt"
        fi
    fi
    local errfile="$WORK/stderr-$NEXTSELF.tmp"
    JOIN_OUT="$(cd "$root" && SOT_COMM_SELF_FILE="$self" "$JOIN" "$@" 2>"$errfile")"
    JOIN_RC=$?
    JOIN_ERR="$(cat "$errfile" 2>/dev/null || true)"
}

registry_has_row() {  # NAME
    jq -e --arg n "$1" '.agents | has($n)' "$SOT_COMM_HOME/registry.json" >/dev/null 2>&1
}

# --- cases -----------------------------------------------------------------

case_with_pins_reaches_the_daemon() {
    mkdir -p "$WORK/proj-a"
    local root; root="$(realpath "$WORK/proj-a")"
    start_stub_daemon
    SOT_WORKSPACE_ID="ws-proj-a" SOT_SOCKET="$SOCK" join_in "$root" --name proj-a-joiner
    local req
    req="$(grep -m1 '"op":"agent.join"' "$REQLOG" 2>/dev/null || true)"
    stop_stub_daemon

    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @proj-a-joiner" || { echo "  stdout: $JOIN_OUT"; return 1; }
    registry_has_row "proj-a-joiner" || { echo "  no registry row for proj-a-joiner"; return 1; }
    [ -n "$req" ] || { echo "  no agent.join request reached the stub daemon"; return 1; }
    printf '%s' "$req" | jq -e '.payload.workspace_id == "ws-proj-a" and .payload.handle == "proj-a-joiner"' >/dev/null \
        || { echo "  agent.join payload mismatch: $req"; return 1; }
    return 0
}

case_without_workspace_id_joins_as_today_with_no_agent_join_sent() {
    mkdir -p "$WORK/proj-b"
    local root; root="$(realpath "$WORK/proj-b")"
    start_stub_daemon
    # SOT_WORKSPACE_ID deliberately unset -- a bare join outside any
    # daemon-spawned workspace (the ordinary "just run it" case) must
    # behave exactly as before this lane: registry join only, no
    # agent.join attempt at all.
    SOT_SOCKET="$SOCK" join_in "$root" --name proj-b-joiner
    local req
    req="$(grep -m1 '"op":"agent.join"' "$REQLOG" 2>/dev/null || true)"
    stop_stub_daemon

    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @proj-b-joiner" || { echo "  stdout: $JOIN_OUT"; return 1; }
    registry_has_row "proj-b-joiner" || { echo "  no registry row for proj-b-joiner"; return 1; }
    [ -z "$req" ] || { echo "  an agent.join request was sent with no SOT_WORKSPACE_ID pinned: $req"; return 1; }
    return 0
}

# Deleted 2026-09-28: a case that unset $SOT_SOCKET to mean "no endpoint".
# The resolver keeps looking past that variable -- a `sotd` on PATH, then a
# /proc scrape -- so on any box running a daemon it found the REAL one and
# sent it a declaration for a workspace that does not exist, every run. It
# failed closed there (the daemon refuses an unknown workspace) and the case
# still passed, which is what hid it. Its assertion is "the registry join
# survives an agent.join that gets no useful answer", and the dead-socket case
# below makes exactly that assertion hermetically.

case_daemon_rejects_agent_join_warns_but_still_joins() {
    mkdir -p "$WORK/proj-d"
    local root; root="$(realpath "$WORK/proj-d")"
    # A reachable daemon that refuses agent.join (simulated here by a
    # socket with nothing listening -- the same "unreachable" shape a
    # stale/wrong SOT_SOCKET produces in the field) must still leave the
    # registry join intact -- agent.join failure is never fatal to
    # comm-join.sh's own job.
    local dead_sock="$WORK/dead.sock"
    SOT_WORKSPACE_ID="ws-proj-d" SOT_SOCKET="$dead_sock" join_in "$root" --name proj-d-joiner

    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @proj-d-joiner" || { echo "  stdout: $JOIN_OUT"; return 1; }
    registry_has_row "proj-d-joiner" || { echo "  no registry row for proj-d-joiner"; return 1; }
    contains "$JOIN_ERR" "could not declare" || { echo "  expected an agent.join warning: $JOIN_ERR"; return 1; }
    return 0
}

case_a_scratch_identity_never_declares_into_the_ambient_row() {
    mkdir -p "$WORK/proj-e"
    local root; root="$(realpath "$WORK/proj-e")"
    # The field case, 2026-09-28: a hermetic test (or a lane) pins its own
    # identity but inherits $SOT_WORKSPACE_ID from the session that
    # launched it. Everything here is pinned for a successful declaration
    # -- a live row id and a reachable daemon -- and the ONE thing that
    # makes it a scratch session is the identity, which names no row. It
    # must join the registry and declare NOTHING: the row belongs to the
    # session whose identity names it, and overwriting that row's handle
    # is what left the owner looking at a grey badge.
    start_stub_daemon
    SOT_COMM_SELF_FILE="$WORK/scratch-self.txt" \
        SOT_WORKSPACE_ID="ws-live-row" SOT_SOCKET="$SOCK" join_in "$root" --name proj-e-scratch
    local req
    req="$(grep -m1 '"op":"agent.join"' "$REQLOG" 2>/dev/null || true)"
    stop_stub_daemon

    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @proj-e-scratch" || { echo "  stdout: $JOIN_OUT"; return 1; }
    registry_has_row "proj-e-scratch" || { echo "  no registry row for proj-e-scratch"; return 1; }
    [ -z "$req" ] || { echo "  a scratch identity declared into the ambient row: $req"; return 1; }
    return 0
}

case_the_identity_names_the_row_not_the_environment() {
    mkdir -p "$WORK/proj-f"
    local root; root="$(realpath "$WORK/proj-f")"
    # The precedence itself, which no other case here can see: the identity
    # names one row and the environment names another. Only the identity may
    # be declared, because the ambient variable is inherited by every child
    # process and is therefore no evidence of whose row this is.
    start_stub_daemon
    SOT_COMM_SELF_FILE="$WORK/${SOT_COMM_TEST_HOST}__ws-from-identity.txt" \
        SOT_WORKSPACE_ID="ws-from-env" SOT_SOCKET="$SOCK" join_in "$root" --name proj-f-joiner
    local req
    req="$(grep -m1 '"op":"agent.join"' "$REQLOG" 2>/dev/null || true)"
    stop_stub_daemon

    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    [ -n "$req" ] || { echo "  no agent.join request reached the stub daemon"; return 1; }
    printf '%s' "$req" | jq -e '.payload.workspace_id == "ws-from-identity"' >/dev/null \
        || { echo "  declared the environment's row, not the identity's: $req"; return 1; }
    return 0
}

# --- run ---------------------------------------------------------------

check "comm-join.sh with SOT_WORKSPACE_ID + SOT_SOCKET pinned declares its handle via agent.join" \
    case_with_pins_reaches_the_daemon
check "comm-join.sh with no SOT_WORKSPACE_ID pinned joins as today and sends no agent.join" \
    case_without_workspace_id_joins_as_today_with_no_agent_join_sent
check "comm-join.sh still joins when the daemon doesn't answer agent.join, with a warning" \
    case_daemon_rejects_agent_join_warns_but_still_joins
check "comm-join.sh with a scratch identity and an inherited SOT_WORKSPACE_ID declares nothing" \
    case_a_scratch_identity_never_declares_into_the_ambient_row
check "comm-join.sh declares the row its identity names, not the one in the environment" \
    case_the_identity_names_the_row_not_the_environment

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
