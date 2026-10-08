#!/usr/bin/env bash
# test-spawn-remote-no-local-row.sh — hermetic suite for the ONE question
# comm-spawn.sh must ask before it writes anything addressable: which BOX
# is this spawn landing on? A spawn against a daemon that declares a
# DIFFERENT host used to write a provisional registry row and an inbox
# file HERE, keyed to THIS host — a false success, because every later
# `comm-send.sh @<handle>` from this box then hit that row, appended to a
# local inbox nothing on the target box reads, and printed
# `filed -> @<handle>` with exit 0.
#
# The daemon's own declared host (`version.query` -> `.payload.daemon.host`,
# `DaemonVersion.host`) is the only trustworthy answer — never the endpoint
# path string, which is a name, not a fact.
#
# Stub unix-socket daemon (nc -klU + FIFO + tail -F, as
# test-spawn-capsule-workspace.sh uses), extended to answer version.query
# with a settable declared host.
#
# Usage: agents/tests/test-spawn-remote-no-local-row.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-wait.sh" || exit 2

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-spawn-remote-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi
export SOT_COMM_HOME="$WORK/home"   # each spawn names its own; nothing falls back to a live one
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
export SOTD_BIN="$(guard_bridge_stub "$WORK/bridge")"
[ -x "$SOTD_BIN" ] || exit 2
SPAWN="$SCRIPTS_DIR/comm-spawn.sh"

# THIS box, for both resolvers: SOT_COMM_TEST_HOST pins comm-context.sh's
# own HOST (registry `host` field, handle derivation), SOT_SELF_HOST pins
# comm-lib.sh's sot_host — the resolver that mirrors Rust's
# state_dir::host_name() and is therefore the one comparable with a
# daemon's declared host. Same value for both so the fixture reads as one
# box.
SELF_HOST_NAME="test-box"
OTHER_HOST_NAME="other-box"
export SOT_COMM_TEST_HOST="$SELF_HOST_NAME"
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

# start_stub_daemon VHOST WSID SLUG LIST_REPLY... — VHOST is the host this
# stub daemon DECLARES in its version.query reply (the whole point of this
# suite). LIST_REPLY... answer successive workspace.list calls in order;
# the last one repeats.
SOCK=""
REQLOG=""
STUBN=0
start_stub_daemon() {
    local vhost="$1" wsid="$2" slug="$3"; shift 3
    local -a list_replies=("$@")
    STUBN=$((STUBN + 1))
    SOCK="$WORK/stub-$STUBN.sock"
    local fifo="$WORK/resp-$STUBN.fifo"
    REQLOG="$WORK/req-$STUBN.log"
    mkfifo "$fifo"
    : > "$REQLOG"

    local hello_reply version_reply create_reply
    hello_reply='{"v":1,"id":1,"kind":"res","op":"hello","payload":{"session_id":"s1","revision":0,"snapshot_pending":false}}'
    version_reply="$(jq -nc --arg h "$vhost" \
        '{v:1,id:1,kind:"res",op:"version.query",payload:{daemon:{app_version:"0.0.0-test",protocol:1,lane_build:"test",lane_proto:1,host:$h,hosts_toml_hash:""},clients:[]}}')"
    create_reply="$(jq -nc --arg id "$wsid" --arg slug "$slug" --arg root "$REPO_PATH" \
        '{v:1,id:1,kind:"res",op:"workspace.create",payload:{workspace_id:$id,slug:$slug,label:$slug,project_root:$root}}')"

    exec 3<>"$fifo"
    nc -klU "$SOCK" < "$fifo" >> "$REQLOG" &
    STUB_NC_PID=$!

    ( local listn=0 idx max=${#list_replies[@]} created=0
      tail -n +1 -F "$REQLOG" 2>/dev/null | while IFS= read -r line; do
        local op; op="$(printf '%s' "$line" | jq -r '.op // empty' 2>/dev/null)"
        case "$op" in
            hello) printf '%s\n' "$hello_reply" >&3 ;;
            version.query) printf '%s\n' "$version_reply" >&3 ;;
            workspace.create) created=1; printf '%s\n' "$create_reply" >&3 ;;
            workspace.list)
                if [ "$created" -eq 0 ]; then
                    printf '%s\n' "{\"v\":1,\"id\":1,\"kind\":\"res\",\"op\":\"workspace.list\",\"payload\":{\"workspaces\":${PRE_CREATE_LIST:-[]}}}" >&3
                    continue
                fi
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

    await test -S "$SOCK" || echo "stub daemon socket never appeared: $SOCK" >&2
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

# entry ID SLUG PHASE — one workspace.list entry for a ready-ish capsule row.
entry() {
    local id="$1" slug="$2" phase="$3"
    jq -nc --arg id "$id" --arg slug "$slug" --arg root "$REPO_PATH" --arg ph "$phase" \
        '[{workspace_id:$id,slug:$slug,label:$slug,project_root:$root,kernel_running:false,is_default:false,autostart_claude:true,agent:"claude",agent_name:"",agent_handle:"",task:"",agent_state:"",agent_summary:"",agent_status_at:"",repl_state:"idle",runtime:"capsule",phase:$ph}]'
}

# run_spawn [ARGS...] — an isolated config path: this session's ambient
# sot-comm env would otherwise leak past the SOT_COMM_HOME override below.
# SOT_SELF_HOST pins what
# THIS box declares itself to be, so the comparison under test is driven
# entirely by the stub's version.query reply.
SPAWNN=0
SPAWN_OUT=""; SPAWN_ERR=""; SPAWN_RC=0; SPAWN_HOME=""
run_spawn() {
    SPAWNN=$((SPAWNN + 1))
    SPAWN_HOME="$WORK/home-$SPAWNN"
    mkdir -p "$SPAWN_HOME"
    local errfile="$WORK/spawn-stderr-$SPAWNN.tmp"
    SPAWN_OUT="$(cd "$WORK" && env -u SOT_WORKSPACE -u SOT_WORKSPACE_ROOT -u SOT_RELAY_ENDPOINT -u SOT_SESSION \
        XDG_CONFIG_HOME="$SPAWN_HOME/xdg-config" \
        SOT_SELF_HOST="$SELF_HOST_NAME" \
        SOT_COMM_HOME="$SPAWN_HOME" SOT_COMM_SELF_FILE="$SPAWN_HOME/self.txt" \
        timeout 30 "$SPAWN" "$REPO_PATH" --endpoint "unix:$SOCK" "$@" 2>"$errfile")"
    SPAWN_RC=$?
    SPAWN_ERR="$(cat "$errfile" 2>/dev/null || true)"
}

registry_has_row() { jq -e --arg n "$1" '.agents | has($n)' "$SPAWN_HOME/registry.json" >/dev/null 2>&1; }
inbox_exists_for() { [ -f "$SPAWN_HOME/inbox/$1.jsonl" ]; }

# --- cases -------------------------------------------------------------

# (a) The defect itself: a daemon on ANOTHER box gets no local row, no
#     local inbox — the handle is NOT addressable from here, and the
#     spawn says so instead of pretending.
case_remote_daemon_writes_nothing_local() {
    local wsid="ws-remote" slug="remote1"
    start_stub_daemon "$OTHER_HOST_NAME" "$wsid" "$slug" "$(entry "$wsid" "$slug" ready)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn --name spawn-remote
    stop_stub_daemon

    [ "$SPAWN_RC" -eq 0 ] || { echo "  exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    registry_has_row "spawn-remote" && {
        echo "  registry row was written for a handle on $OTHER_HOST_NAME — that is the false success"; return 1; }
    inbox_exists_for "spawn-remote" && {
        echo "  local inbox file was created for a handle on $OTHER_HOST_NAME — nothing there reads it"; return 1; }
    contains "$SPAWN_OUT$SPAWN_ERR" "$OTHER_HOST_NAME" || {
        echo "  never named the host the row landed on: $SPAWN_OUT"; return 1; }
    return 0
}

# (b) A DERIVED handle for a remote spawn carries the TARGET box's host,
#     not this one's — a handle naming the wrong box is the same
#     guess-from-a-name defect in another coat.
case_remote_derived_handle_names_target_host() {
    local wsid="ws-derived" slug="derived1"
    start_stub_daemon "$OTHER_HOST_NAME" "$wsid" "$slug" "$(entry "$wsid" "$slug" ready)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn
    stop_stub_daemon

    [ "$SPAWN_RC" -eq 0 ] || { echo "  exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    contains "$SPAWN_OUT" "@repo-$OTHER_HOST_NAME" || {
        echo "  derived handle does not name $OTHER_HOST_NAME: $SPAWN_OUT"; return 1; }
    registry_has_row "repo-$OTHER_HOST_NAME" && { echo "  wrote a local row for a remote handle"; return 1; }
    registry_has_row "repo-$SELF_HOST_NAME" && { echo "  derived the handle against THIS host"; return 1; }
    return 0
}

# (c) The local case is untouched: row + inbox exactly as before.
case_local_daemon_still_writes_both() {
    local wsid="ws-local" slug="local1"
    start_stub_daemon "$SELF_HOST_NAME" "$wsid" "$slug" "$(entry "$wsid" "$slug" ready)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn --name spawn-local
    stop_stub_daemon

    [ "$SPAWN_RC" -eq 0 ] || { echo "  exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    registry_has_row "spawn-local" || { echo "  registry row missing after a LOCAL spawn"; return 1; }
    inbox_exists_for "spawn-local" || { echo "  inbox file missing after a LOCAL spawn"; return 1; }
    return 0
}

# (d) The rollback trap still covers the local write path: a spawn that
#     fails after the row exists deletes it, and SAYS it deleted it.
case_failed_local_spawn_rolls_back() {
    local wsid="ws-lost" slug="lost1"
    start_stub_daemon "$SELF_HOST_NAME" "$wsid" "$slug" "[]"
    SOT_COMM_SPAWN_CAPSULE_WAIT=2 run_spawn --name spawn-lost
    stop_stub_daemon

    [ "$SPAWN_RC" -ne 0 ] || { echo "  unexpectedly succeeded: $SPAWN_OUT"; return 1; }
    contains "$SPAWN_ERR" "rolled back provisional registry row for @spawn-lost" || {
        echo "  rollback was not reported: $SPAWN_ERR"; return 1; }
    registry_has_row "spawn-lost" && { echo "  registry row was NOT rolled back"; return 1; }
    return 0
}

# (e) …and it does not lie about the remote path, where there was never a
#     row to roll back: no "did NOT roll back" verdict on a row this
#     spawn never wrote.
case_failed_remote_spawn_reports_no_rollback() {
    local wsid="ws-rlost" slug="rlost1"
    start_stub_daemon "$OTHER_HOST_NAME" "$wsid" "$slug" "[]"
    SOT_COMM_SPAWN_CAPSULE_WAIT=2 run_spawn --name spawn-rlost
    stop_stub_daemon

    [ "$SPAWN_RC" -ne 0 ] || { echo "  unexpectedly succeeded: $SPAWN_OUT"; return 1; }
    contains "$SPAWN_ERR" "did NOT roll back" && {
        echo "  claimed a verdict on a row it never wrote: $SPAWN_ERR"; return 1; }
    contains "$SPAWN_ERR" "rolled back provisional registry row" && {
        echo "  claimed a rollback of a row it never wrote: $SPAWN_ERR"; return 1; }
    registry_has_row "spawn-rlost" && { echo "  a row exists for a remote handle"; return 1; }
    return 0
}

# --- run -----------------------------------------------------------------

check "daemon declares ANOTHER host: no local registry row, no local inbox" \
    case_remote_daemon_writes_nothing_local
check "daemon declares ANOTHER host: a derived handle names the TARGET host" \
    case_remote_derived_handle_names_target_host
check "daemon declares THIS host: registry row + inbox written, unchanged" \
    case_local_daemon_still_writes_both
check "FAILED local spawn: provisional row rolled back and reported" \
    case_failed_local_spawn_rolls_back
check "FAILED remote spawn: no rollback verdict on a row that was never written" \
    case_failed_remote_spawn_reports_no_rollback

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
