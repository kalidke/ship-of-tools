#!/usr/bin/env bash
# test-spawn-capsule-workspace.sh — hermetic suite for comm-spawn.sh's
# workspace-mode handling of capsule rows. comm-spawn never destroys a
# workspace row itself (ruling: no signal it can observe proves sole
# ownership of a row workspace.create answers with — another caller can
# win the same slug between a list and a create). Stub unix-socket daemon
# (nc -klU + FIFO + tail -F, as test-agent-join.sh uses).
#
# Usage: agents/tests/test-spawn-capsule-workspace.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-wait.sh" || exit 2

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-spawn-capsule-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi

guard_fresh_home "$WORK"; guard_refuse_live_home "$HOME/.sot-comm"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
export SOTD_BIN="$(guard_bridge_stub "$WORK/bridge")"
[ -x "$SOTD_BIN" ] || exit 2
SPAWN="$SCRIPTS_DIR/comm-spawn.sh"

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

    ( local listn=0 idx max=${#list_replies[@]} created=0
      tail -n +1 -F "$REQLOG" 2>/dev/null | while IFS= read -r line; do
        local op; op="$(printf '%s' "$line" | jq -r '.op // empty' 2>/dev/null)"
        case "$op" in
            hello) printf '%s\n' "$hello_reply" >&3 ;;
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

# run_spawn NAME [ARGS...] — an isolated config path: this session's ambient
# sot-comm env would otherwise leak past the SOT_COMM_HOME override below.
# It runs beneath a stand-in capsule (in_row), so comm-spawn's agent check
# ends at that capsule and never depends on the runner's process tree, on
# Linux: on a hosted runner the tree has no capsule, and an OS=Windows_NT case
# reads its top as a Windows walk, which Linux's /proc cannot finish (no winpid).
SPAWNN=0
SPAWN_OUT=""; SPAWN_ERR=""; SPAWN_RC=0; SPAWN_HOME=""
run_spawn() {
    local name="$1"; shift
    SPAWNN=$((SPAWNN + 1))
    SPAWN_HOME="$WORK/home-$SPAWNN"
    mkdir -p "$SPAWN_HOME"
    local errfile="$WORK/spawn-stderr-$SPAWNN.tmp"
    SPAWN_OUT="$(cd "$WORK" && in_row spawn-test env -u SOT_WORKSPACE -u SOT_WORKSPACE_ROOT -u SOT_SESSION \
        ${SPAWN_PATH:+PATH="$SPAWN_PATH"} GUARD_PIPE_SOCKET="$SOCK" XDG_CONFIG_HOME="$SPAWN_HOME/xdg-config" \
        SOT_COMM_HOME="$SPAWN_HOME" SOT_COMM_SELF_FILE="$SPAWN_HOME/self.txt" \
        timeout 30 "$SPAWN" ${name:+--name "$name"} "$REPO_PATH" --endpoint "${SPAWN_EP:-unix:$SOCK}" "$@" 2>"$errfile")"
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

# One repo root, one workspace: a root the daemon already lists is refused
# before any registry write or workspace.create.
assert_occupied_root_refused() {
    local who="$1"
    [ "$SPAWN_RC" -eq 1 ] || { echo "  exited $SPAWN_RC (want 1): $SPAWN_ERR"; return 1; }
    contains "$SPAWN_ERR" "already has a workspace: 'repo' (slug 'repo', id ws-occ)" \
        || { echo "  stderr: $SPAWN_ERR"; return 1; }
    contains "$SPAWN_ERR" "comm-worktree-new.sh" || { echo "  no worktree pointer: $SPAWN_ERR"; return 1; }
    ! grep -q '"op":"workspace.create"' "$REQLOG" || { echo "  workspace.create was sent"; return 1; }
    [ ! -e "$SPAWN_HOME/registry.json" ] || jq -e '(.agents // {}) == {}' "$SPAWN_HOME/registry.json" >/dev/null \
        || { echo "  a registry row was written"; return 1; }
    ! contains "$SPAWN_ERR" "roll" || { echo "  rollback verdict printed for a row never written: $SPAWN_ERR"; return 1; }
    if [ -n "$who" ]; then
        [ ! -f "$SPAWN_HOME/inbox/$who.jsonl" ] || { echo "  inbox file created for $who"; return 1; }
    fi
    return 0
}

case_occupied_root_refused_explicit_name() {
    PRE_CREATE_LIST="$(entry ws-occ repo capsule ready)"
    start_stub_daemon ws-occ repo "$(entry ws-occ repo capsule ready)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn spawn-occ
    stop_stub_daemon
    PRE_CREATE_LIST=""
    assert_occupied_root_refused spawn-occ
}

case_occupied_root_refused_derived_name() {
    PRE_CREATE_LIST="$(entry ws-occ repo capsule ready)"
    start_stub_daemon ws-occ repo "$(entry ws-occ repo capsule ready)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn ""
    stop_stub_daemon
    PRE_CREATE_LIST=""
    assert_occupied_root_refused ""
}

# stub_windows_tools map|fail — what a Windows box has on PATH for a pipe:
# endpoint: the stand-in bridge (carries stdin to the stub daemon's socket and its
# replies back), cmd (answers the hello's account lookup) and cygpath. `map` acts as cygpath -m for this test (/x ->
# C:/mapped/x; a C:/ path comes back unchanged); `fail` exits 1. Call it AFTER
# start_stub_daemon: GUARD_PIPE_SOCKET names the current $SOCK.
stub_windows_tools() {
    mkdir -p "$WORK/bin"
    case "$1" in
        map) cat > "$WORK/bin/cygpath" <<'EOF'
#!/usr/bin/env bash
[ "$1" = "-m" ] && shift
case "$1" in C:/*) printf '%s\n' "$1" ;; *) printf 'C:/mapped%s\n' "$1" ;; esac
EOF
            ;;
        fail) printf '#!/usr/bin/env bash\nexit 1\n' > "$WORK/bin/cygpath" ;;
    esac
    # `cmd //c "whoami /user /fo csv /nh"`, the hello's `os_user` on Windows (comm-lib-client.sh `_sot_os_user`).
    printf '#!/bin/sh\nprintf '"'"'"fakehost\\\\fakeuser","S-1-5-21-1-2-3-1001"\\r\\n'"'"'\n' > "$WORK/bin/cmd"
    chmod +x "$WORK/bin/cygpath" "$WORK/bin/cmd"
}
wire_root() { jq -r 'select(.op=="workspace.create") | .payload.project_root' "$REQLOG"; }

# A Windows box's daemon listens only on a named pipe: the request goes
# through the stand-in bridge (stubbed here by a script that carries stdin to
# the stub daemon's socket and its replies back). OS stays unset: the pipe:
# endpoint alone makes the request carry cygpath -m's spelling.
case_capsule_ready_over_pipe_endpoint() {
    local wsid="ws-pipe" slug="pipe1"
    start_stub_daemon "$wsid" "$slug" \
        "$(entry "$wsid" "$slug" capsule starting)" \
        "$(entry "$wsid" "$slug" capsule ready)"
    stub_windows_tools map
    SPAWN_EP='pipe:\\.\pipe\sot-stub' SPAWN_PATH="$WORK/bin:$PATH" \
        SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn spawn-pipe
    stop_stub_daemon

    [ "$SPAWN_RC" -eq 0 ] || { echo "  exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    contains "$SPAWN_OUT" "Capsule row ready" || { echo "  stdout: $SPAWN_OUT"; return 1; }
    registry_has_row "spawn-pipe" || { echo "  registry row missing after success"; return 1; }
    local got; got="$(wire_root)"; [ "$got" = "C:/mapped$(realpath "$REPO_PATH")" ] || { echo "  project_root on the wire: '$got'"; return 1; }
    return 0
}

# The spelling is the daemon's, not the caller's: a Windows caller reaching a
# non-pipe daemon sends the path as resolved here.
case_windows_caller_nonpipe_daemon_keeps_root() {
    local wsid="ws-win" slug="win1"
    start_stub_daemon "$wsid" "$slug" \
        "$(entry "$wsid" "$slug" capsule starting)" \
        "$(entry "$wsid" "$slug" capsule ready)"
    stub_windows_tools map
    OS=Windows_NT SPAWN_PATH="$WORK/bin:$PATH" \
        SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn spawn-win
    stop_stub_daemon

    [ "$SPAWN_RC" -eq 0 ] || { echo "  exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    local got; got="$(wire_root)"
    [ "$got" = "$(realpath "$REPO_PATH")" ] || { echo "  project_root on the wire: '$got'"; return 1; }
}

# A failed conversion refuses before the derived-name claim, the earliest
# registry write: nothing claimed, nothing to roll back.
case_pipe_conversion_failure_refuses_before_any_write() {
    start_stub_daemon ws-cvt cvt1 "$(entry ws-cvt cvt1 capsule ready)"
    stub_windows_tools fail
    OS=Windows_NT SPAWN_EP='pipe:\\.\pipe\sot-stub' SPAWN_PATH="$WORK/bin:$PATH" \
        SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn ""
    stop_stub_daemon
    [ "$SPAWN_RC" -eq 1 ] || { echo "  exited $SPAWN_RC (want 1): $SPAWN_ERR"; return 1; }
    contains "$SPAWN_ERR" "(cygpath -m failed); nothing was spawned." || { echo "  stderr: $SPAWN_ERR"; return 1; }
    ! contains "$SPAWN_ERR" "roll" || { echo "  rollback verdict printed: $SPAWN_ERR"; return 1; }
    [ ! -e "$SPAWN_HOME/registry.json" ] || jq -e '(.agents // {}) == {}' "$SPAWN_HOME/registry.json" >/dev/null \
        || { echo "  a registry row was written"; return 1; }
    ! grep -q '"op":"workspace.create"' "$REQLOG" || { echo "  workspace.create was sent"; return 1; }
    return 0
}

# The daemon lists the C:/ form it was sent; the occupancy check compares in
# that spelling, so the early refusal still fires.
case_pipe_occupied_root_in_daemon_spelling_refused() {
    PRE_CREATE_LIST="$(REPO_PATH="C:/mapped$(realpath "$REPO_PATH")" entry ws-occ repo capsule ready)"
    start_stub_daemon ws-occ repo "$(entry ws-occ repo capsule ready)"
    stub_windows_tools map
    SPAWN_EP='pipe:\\.\pipe\sot-stub' SPAWN_PATH="$WORK/bin:$PATH" \
        SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn ""
    stop_stub_daemon
    PRE_CREATE_LIST=""
    assert_occupied_root_refused ""
}

# A bash row (--agent none) runs a shell and no agent, so nothing joins comm:
# the spawn claims no handle, writes no registry row or inbox, sends an empty
# agent_name, waits for ready like any row and prints the workspace id last.
case_bash_row_spawns_with_no_handle() {
    local wsid="ws-bash" slug="bash1" create last
    start_stub_daemon "$wsid" "$slug" \
        "$(entry "$wsid" "$slug" capsule starting)" \
        "$(entry "$wsid" "$slug" capsule ready)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn "" --agent none
    create="$(jq -c 'select(.op=="workspace.create") | .payload | [.agent, .agent_name, .autostart_claude]' "$REQLOG")"
    stop_stub_daemon
    last="$(printf '%s\n' "$SPAWN_OUT" | tail -n 1)"

    [ "$SPAWN_RC" -eq 0 ] || { echo "  exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    [ "$create" = '["none","",false]' ] || { echo "  workspace.create [agent, agent_name, autostart_claude]: $create"; return 1; }
    contains "$last" "(id=$wsid)" || { echo "  last stdout line: $last"; return 1; }
    contains "$last" "comm-despawn.sh $wsid" || { echo "  last stdout line: $last"; return 1; }
    ! contains "$SPAWN_OUT" "Spawned @" || { echo "  agent text printed for a bash row: $SPAWN_OUT"; return 1; }
    ! contains "$SPAWN_OUT" "addressable" || { echo "  agent text printed for a bash row: $SPAWN_OUT"; return 1; }
    [ ! -e "$SPAWN_HOME/registry.json" ] || jq -e '(.agents // {}) == {}' "$SPAWN_HOME/registry.json" >/dev/null \
        || { echo "  a registry row was written: $(jq -c .agents "$SPAWN_HOME/registry.json")"; return 1; }
    [ -z "$(ls -A "$SPAWN_HOME/inbox" 2>/dev/null)" ] || { echo "  an inbox file was written: $(ls "$SPAWN_HOME/inbox")"; return 1; }
    ! contains "$SPAWN_ERR" "roll" || { echo "  rollback verdict printed: $SPAWN_ERR"; return 1; }
    return 0
}

# --name, a name argument (the legacy form) and --task each promise an agent:
# a bash row refuses them before workspace.create or any registry write.
case_bash_row_refuses_a_handle_or_a_task() {
    local how
    for how in name positional task; do
        start_stub_daemon ws-bash2 bash2 "$(entry ws-bash2 bash2 capsule ready)"
        case "$how" in
            name)       SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn bash-name --agent none ;;
            positional) SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn "" --agent none "$REPO_PATH" ;;
            task)       SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn "" --agent none --task "do x" ;;
        esac
        stop_stub_daemon
        [ "$SPAWN_RC" -eq 1 ] || { echo "  $how: exited $SPAWN_RC (want 1): $SPAWN_ERR"; return 1; }
        contains "$SPAWN_ERR" "--agent none starts a bash row" || { echo "  $how: stderr: $SPAWN_ERR"; return 1; }
        ! grep -q '"op":"workspace.create"' "$REQLOG" || { echo "  $how: workspace.create was sent"; return 1; }
        [ ! -e "$SPAWN_HOME/registry.json" ] || jq -e '(.agents // {}) == {}' "$SPAWN_HOME/registry.json" >/dev/null \
            || { echo "  $how: a registry row was written"; return 1; }
        [ -z "$(ls -A "$SPAWN_HOME/inbox" 2>/dev/null)" ] || { echo "  $how: an inbox file was written"; return 1; }
    done
    return 0
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
check "occupied root, explicit --name: refused before any write or create" \
    case_occupied_root_refused_explicit_name
check "occupied root, derived name: refused before any write or create" \
    case_occupied_root_refused_derived_name
check "pipe: endpoint (Windows local daemon): spawn succeeds and workspace.create carries cygpath -m's spelling" \
    case_capsule_ready_over_pipe_endpoint
check "Windows caller, non-pipe daemon: project_root goes unchanged (the daemon's spelling, not the caller's)" \
    case_windows_caller_nonpipe_daemon_keeps_root
check "pipe: endpoint, cygpath fails: refused before any registry write (nothing claimed, nothing rolled back)" \
    case_pipe_conversion_failure_refuses_before_any_write
check "pipe: endpoint, root listed in the daemon's C:/ spelling: refused before any write or create" \
    case_pipe_occupied_root_in_daemon_spelling_refused

check "--agent none: a bash row with no handle, registry row or inbox; waits for ready, prints its id" \
    case_bash_row_spawns_with_no_handle
check "--agent none refuses --name, a name argument and --task before any create or write" \
    case_bash_row_refuses_a_handle_or_a_task
echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
