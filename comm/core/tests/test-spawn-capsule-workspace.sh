#!/usr/bin/env bash
# test-spawn-capsule-workspace.sh — hermetic suite for comm-spawn.sh's
# workspace-mode handling of capsule rows. comm-spawn never destroys a
# workspace row itself (ruling: no signal it can observe proves sole
# ownership of a row workspace.create answers with — another caller can
# win the same slug between a list and a create). Stub unix-socket daemon
# (nc -klU + FIFO + tail -F, as test-agent-join.sh uses).
#
# Usage: comm/core/tests/test-spawn-capsule-workspace.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
SPAWN="$SCRIPTS_DIR/comm-spawn.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-spawn-capsule-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi

export SOT_COMM_TEST_HOST="test-host"
unset SOT_WORKSPACE_ID   # never let an ambient row leak into the self-file slot
mkdir -p "$WORK/repo"
REPO_PATH="$WORK/repo"

STUB_NC_PID=""
STUB_WATCHER_PID=""
cleanup() {
    [ -n "$STUB_WATCHER_PID" ] && pkill -TERM -P "$STUB_WATCHER_PID" >/dev/null 2>&1
    [ -n "$STUB_WATCHER_PID" ] && kill "$STUB_WATCHER_PID" >/dev/null 2>&1
    [ -n "$STUB_NC_PID" ] && kill "$STUB_NC_PID" >/dev/null 2>&1
    exec 3>&- 2>/dev/null || true
    rm -rf "$WORK"
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

# start_stub_daemon WSID SLUG LIST_REPLY... — LIST_REPLY... answer
# successive workspace.list calls in order; the last one repeats. A
# workspace.destroy request is logged to REQLOG (for the never-sent
# assertions below) but the stub answers none — comm-spawn must never
# send one.
SOCK=""
REQLOG=""
STUBN=0
start_stub_daemon() {
    local wsid="$1" slug="$2"; shift 2
    local -a list_replies=("$@")
    STUBN=$((STUBN + 1))
    SOCK="$WORK/stub-$STUBN.sock"
    local fifo="$WORK/resp-$STUBN.fifo"
    REQLOG="$WORK/req-$STUBN.log"
    mkfifo "$fifo"
    : > "$REQLOG"

    local hello_reply create_reply
    hello_reply='{"v":1,"id":1,"kind":"res","op":"hello","payload":{"session_id":"s1","revision":0,"snapshot_pending":false}}'
    create_reply="$(jq -nc --arg id "$wsid" --arg slug "$slug" --arg root "$REPO_PATH" \
        '{v:1,id:1,kind:"res",op:"workspace.create",payload:{workspace_id:$id,slug:$slug,label:$slug,project_root:$root}}')"

    exec 3<>"$fifo"
    nc -klU "$SOCK" < "$fifo" >> "$REQLOG" &
    STUB_NC_PID=$!

    ( local listn=0 idx max=${#list_replies[@]}
      tail -n +1 -F "$REQLOG" 2>/dev/null | while IFS= read -r line; do
        local op; op="$(printf '%s' "$line" | jq -r '.op // empty' 2>/dev/null)"
        case "$op" in
            hello) printf '%s\n' "$hello_reply" >&3 ;;
            workspace.create) printf '%s\n' "$create_reply" >&3 ;;
            workspace.list)
                listn=$((listn + 1))
                idx=$listn
                [ "$idx" -gt "$max" ] && idx=$max
                if [ "$idx" -ge 1 ]; then
                    printf '%s\n' "{\"v\":1,\"id\":1,\"kind\":\"res\",\"op\":\"workspace.list\",\"payload\":{\"workspaces\":${list_replies[$((idx - 1))]}}}" >&3
                else
                    printf '%s\n' '{"v":1,"id":1,"kind":"res","op":"workspace.list","payload":{"workspaces":[]}}' >&3
                fi
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

# entry ID SLUG RUNTIME PHASE — one workspace.list entry, PHASE may be "".
entry() {
    local id="$1" slug="$2" runtime="$3" phase="$4"
    if [ -n "$phase" ]; then
        jq -nc --arg id "$id" --arg slug "$slug" --arg root "$REPO_PATH" --arg rt "$runtime" --arg ph "$phase" \
            '[{workspace_id:$id,slug:$slug,label:$slug,project_root:$root,kernel_running:false,is_default:false,autostart_claude:true,agent:"claude",agent_name:"",agent_handle:"",task:"",agent_state:"",agent_summary:"",agent_status_at:"",repl_state:"idle",runtime:$rt,phase:$ph}]'
    else
        jq -nc --arg id "$id" --arg slug "$slug" --arg root "$REPO_PATH" --arg rt "$runtime" \
            '[{workspace_id:$id,slug:$slug,label:$slug,project_root:$root,kernel_running:false,is_default:false,autostart_claude:true,agent:"claude",agent_name:"",agent_handle:"",task:"",agent_state:"",agent_summary:"",agent_status_at:"",repl_state:"idle",runtime:$rt}]'
    fi
}

# run_spawn NAME [ARGS...] — a dummy token + isolated config path: an
# unset SOT_TOKEN falls back to the REAL ~/.config/sot/token
# (comm-lib.sh sot_hello_frame); this session's ambient sot-comm env
# would otherwise leak past the SOT_COMM_HOME override below.
SPAWNN=0
SPAWN_OUT=""; SPAWN_ERR=""; SPAWN_RC=0; SPAWN_HOME=""
run_spawn() {
    local name="$1"; shift
    SPAWNN=$((SPAWNN + 1))
    SPAWN_HOME="$WORK/home-$SPAWNN"
    mkdir -p "$SPAWN_HOME"
    local errfile="$WORK/spawn-stderr-$SPAWNN.tmp"
    SPAWN_OUT="$(cd "$WORK" && env -u SOT_WORKSPACE -u SOT_WORKSPACE_ROOT -u SOT_RELAY_ENDPOINT -u SOT_SESSION \
        SOT_TOKEN="dummy-test-token" XDG_CONFIG_HOME="$SPAWN_HOME/xdg-config" \
        SOT_COMM_HOME="$SPAWN_HOME" SOT_COMM_SELF_FILE="$SPAWN_HOME/self.txt" \
        timeout 30 "$SPAWN" --name "$name" "$REPO_PATH" --endpoint "unix:$SOCK" "$@" 2>"$errfile")"
    SPAWN_RC=$?
    SPAWN_ERR="$(cat "$errfile" 2>/dev/null || true)"
}

registry_has_row() { jq -e --arg n "$1" '.agents | has($n)' "$SPAWN_HOME/registry.json" >/dev/null 2>&1; }
destroy_was_sent_for() { grep -q "\"op\":\"workspace.destroy\".*\"workspace_id\":\"$1\"" "$REQLOG" 2>/dev/null; }
despawn_cmd_printed_for() { contains "$SPAWN_ERR" "comm-despawn.sh $1"; }

# assert_never_destroyed WSID NAME — the shared shape every failure case
# checks: non-zero exit, no destroy request, the despawn command printed,
# and the provisional registry handle (this spawn's own) rolled back.
assert_never_destroyed() {
    local wsid="$1" name="$2"
    [ "$SPAWN_RC" -ne 0 ] || { echo "  unexpectedly succeeded: $SPAWN_OUT"; return 1; }
    destroy_was_sent_for "$wsid" && { echo "  workspace.destroy was sent — comm-spawn must never destroy a row"; return 1; }
    despawn_cmd_printed_for "$wsid" || { echo "  no despawn command printed: $SPAWN_ERR"; return 1; }
    registry_has_row "$name" && { echo "  registry row was NOT rolled back"; return 1; }
    return 0
}

# --- cases -------------------------------------------------------------

case_capsule_reaches_ready_on_second_poll() {
    local wsid="ws-ready" slug="ready1"
    start_stub_daemon "$wsid" "$slug" \
        "$(entry "$wsid" "$slug" capsule starting)" \
        "$(entry "$wsid" "$slug" capsule ready)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn spawn-ready
    stop_stub_daemon

    [ "$SPAWN_RC" -eq 0 ] || { echo "  exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    contains "$SPAWN_OUT" "Capsule row ready" || { echo "  stdout: $SPAWN_OUT"; return 1; }
    registry_has_row "spawn-ready" || { echo "  registry row missing after success"; return 1; }
    return 0
}

case_capsule_terminal_phase_never_destroys() {
    local wsid="ws-term" slug="term1"
    start_stub_daemon "$wsid" "$slug" "$(entry "$wsid" "$slug" capsule ended_no_respawn)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn spawn-term
    local out; out="$SPAWN_ERR"
    stop_stub_daemon

    contains "$out" "ended_no_respawn" || { echo "  stderr: $out"; return 1; }
    assert_never_destroyed "$wsid" spawn-term
}

case_capsule_timeout_never_destroys() {
    local wsid="ws-slow" slug="slow1"
    start_stub_daemon "$wsid" "$slug" "$(entry "$wsid" "$slug" capsule starting)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=2 run_spawn spawn-slow
    local out; out="$SPAWN_ERR"
    stop_stub_daemon

    contains "$out" "still starting" || { echo "  stderr: $out"; return 1; }
    assert_never_destroyed "$wsid" spawn-slow
}

case_list_never_reports_id_never_destroys() {
    local wsid="ws-nolist" slug="nolist1"
    start_stub_daemon "$wsid" "$slug" "[]"
    SOT_COMM_SPAWN_CAPSULE_WAIT=2 run_spawn spawn-nolist
    local out; out="$SPAWN_ERR"
    stop_stub_daemon

    contains "$out" "never reported id=$wsid" || { echo "  stderr: $out"; return 1; }
    assert_never_destroyed "$wsid" spawn-nolist
}

# --- run -----------------------------------------------------------------

check "capsule row reaches phase 'ready' on the second poll: succeeds" \
    case_capsule_reaches_ready_on_second_poll
check "capsule row settles to 'ended_no_respawn': never destroys, prints the despawn command" \
    case_capsule_terminal_phase_never_destroys
check "capsule row never reaches 'ready' within the wait: TIMEOUT never destroys" \
    case_capsule_timeout_never_destroys
check "workspace.list never reports the created id: never destroys" \
    case_list_never_reports_id_never_destroys

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
