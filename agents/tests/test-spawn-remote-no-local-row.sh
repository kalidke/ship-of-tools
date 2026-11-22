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

# Record the arguments the local CLI receives. In the older CLI, extra arguments to derive returned usage (2).
# OLD_DERIVE=true makes this stub answer as that CLI did.
export BUILT_SOTD="$GUARD_SOTD"
GUARD_SOTD="$WORK/record-comm"
cat > "$GUARD_SOTD" <<'STUB'
#!/usr/bin/env bash
if [ "$1 ${2:-}" = "comm derive" ]; then
    printf '%s\n' "${*:5}" >> "$SOT_COMM_HOME/derive.log"
    if [ "${OLD_DERIVE:-}" = true ] && [ "${6:-}" = --agent ]; then
        echo "usage: sotd comm derive <reclaim|fresh> <root> <raw host> [--claim]" >&2
        exit 2
    fi
    if [ "${BREAK_REGISTRY:-}" = true ]; then
        printf 'invalid JSON\n' > "$SOT_COMM_HOME/registry.json"
    fi
fi
exec "${BUILT_SOTD:?a built sotd is required}" "$@"
STUB
chmod +x "$GUARD_SOTD" || exit 2

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
    end_group "$STUB_WATCHER_PID"
    end_group "$STUB_NC_PID"
    { exec 3>&-; } 2>/dev/null || true
    rm -rf "${WORK:?}"
}
trap cleanup EXIT

PASS=0
FAIL=0
check() {
    local desc="$1" rc
    shift
    "$@"
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
    trap - EXIT; set -m   # each step leads a process group of its own, which end_group (lib-wait.sh) ends whole
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
    set +m; trap cleanup EXIT   # the steps are forked with no EXIT trap: a stop that reaches one before bash resets the copy would run cleanup in it

    await test -S "$SOCK" || echo "stub daemon socket never appeared: $SOCK" >&2
}

stop_stub_daemon() {
    end_group "$STUB_WATCHER_PID"
    end_group "$STUB_NC_PID"
    { exec 3>&-; } 2>/dev/null || true
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
    SPAWN_OUT="$(cd "$WORK" && bounded 30 env -u SOT_WORKSPACE -u SOT_WORKSPACE_ROOT -u SOT_SESSION \
        XDG_CONFIG_HOME="$SPAWN_HOME/xdg-config" \
        SOT_SELF_HOST="$SELF_HOST_NAME" \
        SOT_COMM_HOME="$SPAWN_HOME" SOT_COMM_SELF_FILE="$SPAWN_HOME/self.txt" \
        "$SPAWN" "$REPO_PATH" --endpoint "unix:$SOCK" "$@" 2>"$errfile")"
    SPAWN_RC=$?
    SPAWN_ERR="$(cat "$errfile" 2>/dev/null || true)"
}

registry_has_row() { jq -e --arg n "$1" '.agents | has($n)' "$SPAWN_HOME/registry.json" >/dev/null 2>&1; }
inbox_exists_for() { [ -f "$SPAWN_HOME/inbox/$1.jsonl" ]; }
created_as() {
    jq -se --arg kind "$1" --arg name "$2" \
        '[.[] | select(.op == "workspace.create") | [.payload.agent, .payload.agent_name]] == [[$kind, $name]]' "$REQLOG" >/dev/null
}

# --- cases -------------------------------------------------------------

# (a) The defect itself: a daemon on ANOTHER box gets no local row, no
#     local inbox — the handle is NOT addressable from here, and the
#     spawn says so instead of pretending.
case_remote_daemon_writes_nothing_local() {
    local wsid="ws-remote" slug="remote1"
    start_stub_daemon "$OTHER_HOST_NAME" "$wsid" "$slug" "$(entry "$wsid" "$slug" ready)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn --name spawn-remote --agent codex
    stop_stub_daemon

    [ "$SPAWN_RC" -eq 0 ] || { echo "  exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    created_as codex spawn-remote && [ ! -e "$SPAWN_HOME/derive.log" ] || { echo "  explicit handle was derived or changed"; return 1; }
    registry_has_row "spawn-remote" && {
        echo "  registry row was written for a handle on $OTHER_HOST_NAME — that is the false success"; return 1; }
    inbox_exists_for "spawn-remote" && {
        echo "  local inbox file was created for a handle on $OTHER_HOST_NAME — nothing there reads it"; return 1; }
    contains "$SPAWN_OUT$SPAWN_ERR" "$OTHER_HOST_NAME" || {
        echo "  never named the host the row landed on: $SPAWN_OUT"; return 1; }
    return 0
}

# (b) The target only needs workspace.create's existing agent_name. Both paths run derive on the spawning box.
case_derived_handle() {
    local host="$1" kind="$2" name="$3" calls="$4" wsid="ws-derived" slug="derived1" actual
    start_stub_daemon "$host" "$wsid" "$slug" "$(entry "$wsid" "$slug" ready)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn --agent "$kind"
    stop_stub_daemon

    [ "$SPAWN_RC" -eq 0 ] || { echo "  exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    actual="$(cat "$SPAWN_HOME/derive.log")"
    [ "$actual" = "$calls" ] || { echo "  expected derive arguments '$calls', got '$actual'"; return 1; }
    contains "$SPAWN_OUT" "@$name" && created_as "$kind" "$name" || { echo "  expected $kind row @$name: $SPAWN_OUT"; return 1; }
    if [ "$host" = "$SELF_HOST_NAME" ]; then
        registry_has_row "$name"
    else
        jq -e '.agents == {}' "$SPAWN_HOME/registry.json" >/dev/null
    fi
}

# (b2) An older local sotd does not know --agent. A Claude child is asked in the old form and spawns as before; a Codex
#      child stops before anything is written, with one line saying so, on both paths.
case_older_sotd_codex_refused() {
    local host="$1"
    start_stub_daemon "$host" ws-old old1 "$(entry ws-old old1 ready)"
    OLD_DERIVE=true run_spawn --agent codex
    stop_stub_daemon

    [ "$SPAWN_RC" -eq 1 ] && contains "$SPAWN_ERR" "installed sotd is older than this script and cannot derive a Codex session's handle" &&
        [ "$(cat "$SPAWN_HOME/derive.log")" = "$host --agent codex$([ "$host" = "$SELF_HOST_NAME" ] && echo ' --claim')" ] &&
        jq -se 'all(.[]; .op != "workspace.create")' "$REQLOG" >/dev/null &&
        jq -e '.agents == {}' "$SPAWN_HOME/registry.json" >/dev/null || { echo "  rc $SPAWN_RC: $SPAWN_ERR"; return 1; }
}

case_remote_derive_failure() {
    start_stub_daemon "$OTHER_HOST_NAME" ws-read read1 "$(entry ws-read read1 ready)"
    BREAK_REGISTRY=true run_spawn --agent codex
    stop_stub_daemon

    [ "$SPAWN_RC" -ne 0 ] && contains "$SPAWN_ERR" "registry could not be read" &&
        [ "$(cat "$SPAWN_HOME/derive.log")" = "$OTHER_HOST_NAME --agent codex" ] &&
        jq -se 'all(.[]; .op != "workspace.create")' "$REQLOG" >/dev/null
}

# (c) The local case writes its registry row through the daemon and touches no inbox file: the inbox is the daemon's
#     (agents/CLAUDE.md, guarantee 9), and its first append makes it.
case_local_daemon_writes_the_row_and_no_inbox() {
    local wsid="ws-local" slug="local1"
    start_stub_daemon "$SELF_HOST_NAME" "$wsid" "$slug" "$(entry "$wsid" "$slug" ready)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn --name spawn-local
    stop_stub_daemon

    [ "$SPAWN_RC" -eq 0 ] || { echo "  exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    registry_has_row "spawn-local" || { echo "  registry row missing after a LOCAL spawn"; return 1; }
    ! inbox_exists_for "spawn-local" || { echo "  the spawn made the inbox file itself"; return 1; }
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

# (f) A daemon that declares no host (one that predates the field answers without it): the spawn stays LOCAL, writes
#     its row, and says it assumed this box.
case_undeclared_host_stays_local() {
    local wsid="ws-undecl" slug="undecl1"
    start_stub_daemon "" "$wsid" "$slug" "$(entry "$wsid" "$slug" ready)"
    SOT_COMM_SPAWN_CAPSULE_WAIT=10 run_spawn --name spawn-undecl
    stop_stub_daemon

    [ "$SPAWN_RC" -eq 0 ] || { echo "  exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    registry_has_row "spawn-undecl" || { echo "  no registry row for a daemon that declares no host"; return 1; }
    contains "$SPAWN_ERR" "did not declare a host" || { echo "  the assumption was not said: $SPAWN_ERR"; return 1; }
    return 0
}

# --- run -----------------------------------------------------------------

check "daemon declares ANOTHER host: no local registry row, no local inbox" \
    case_remote_daemon_writes_nothing_local
check "unnamed Claude child keeps the target host's handle" \
    case_derived_handle "$OTHER_HOST_NAME" claude "repo-$OTHER_HOST_NAME" "$OTHER_HOST_NAME"
check "unnamed local Claude child derives with the argv it always had" \
    case_derived_handle "$SELF_HOST_NAME" claude "repo-$SELF_HOST_NAME" "$SELF_HOST_NAME --claim"
check "unnamed local Codex child claims its own kind's handle" \
    case_derived_handle "$SELF_HOST_NAME" codex "repo-cx-$SELF_HOST_NAME" "$SELF_HOST_NAME --agent codex --claim"
check "unnamed remote Codex child sends its kind's handle to the older target" \
    case_derived_handle "$OTHER_HOST_NAME" codex "repo-cx-$OTHER_HOST_NAME" "$OTHER_HOST_NAME --agent codex"
check "an older sotd stops an unnamed remote Codex spawn with one line" case_older_sotd_codex_refused "$OTHER_HOST_NAME"
check "an older sotd stops an unnamed local Codex spawn with one line, claiming nothing" case_older_sotd_codex_refused "$SELF_HOST_NAME"
OLD_DERIVE=true check "an older sotd still spawns an unnamed Claude child as before" \
    case_derived_handle "$OTHER_HOST_NAME" claude "repo-$OTHER_HOST_NAME" "$OTHER_HOST_NAME"
check "a registry read failure ends a remote spawn after one derivation" case_remote_derive_failure
check "daemon declares THIS host: registry row written through the daemon, no inbox file touched" \
    case_local_daemon_writes_the_row_and_no_inbox
check "FAILED local spawn: provisional row rolled back and reported" \
    case_failed_local_spawn_rolls_back
check "FAILED remote spawn: no rollback verdict on a row that was never written" \
    case_failed_remote_spawn_reports_no_rollback
check "daemon declares NO host: the spawn stays local, writes its row and says it assumed this box" \
    case_undeclared_host_stays_local

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
