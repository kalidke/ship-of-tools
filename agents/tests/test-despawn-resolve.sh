#!/usr/bin/env bash
# test-despawn-resolve.sh — hermetic suite for comm-despawn.sh's resolve-first
# flow: a name that resolves to no workspace fails loudly (exit 1) and changes
# nothing, and the registry row is removed only after a confirmed destroy.
# Also covers comm-worktree-clean.sh, the one caller that despawned twice.
# Stub unix-socket daemon (nc -klU + FIFO + tail -F, as
# test-spawn-capsule-workspace.sh uses).
#
# Usage: agents/tests/test-despawn-resolve.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-wait.sh" || exit 2

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-despawn-resolve-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi

guard_fresh_home "$WORK"; guard_refuse_live_home "$HOME/.sot-comm"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
export SOTD_BIN="$(guard_bridge_stub "$WORK/bridge")"
[ -x "$SOTD_BIN" ] || exit 2

export SOT_COMM_TEST_HOST="test-host"
# This box's declared host (sot_host): the registry rows seeded here and the stub daemon's version reply carry it.
export SOT_SELF_HOST="test-host"
unset SOT_WORKSPACE_ID

STUB_NC_PID=""; STUB_WATCHER_PID=""
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

# start_stub_daemon — answers hello, workspace.list (LIST_PAYLOAD) and
# workspace.destroy (DESTROY_PAYLOAD), both captured at start; every request
# is logged to REQLOG.
SOCK=""; REQLOG=""; STUBN=0
LIST_PAYLOAD='{"workspaces":[]}'
DESTROY_PAYLOAD='{"error":"refused","code":"test_refused"}'
start_stub_daemon() {
    STUBN=$((STUBN + 1))
    SOCK="$WORK/stub-$STUBN.sock"
    local fifo="$WORK/resp-$STUBN.fifo"
    REQLOG="$WORK/req-$STUBN.log"
    mkfifo "$fifo"
    : > "$REQLOG"

    local hello_reply list_reply destroy_reply version_reply
    version_reply="{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"version.query\",\"payload\":{\"daemon\":{\"host\":\"${STUB_DECLARED_HOST-test-host}\"}}}"
    hello_reply='{"v":1,"id":1,"kind":"res","op":"hello","payload":{"session_id":"s1","revision":0,"snapshot_pending":false}}'
    list_reply="{\"v\":1,\"id\":1,\"kind\":\"res\",\"op\":\"workspace.list\",\"payload\":$LIST_PAYLOAD}"
    destroy_reply="{\"v\":1,\"id\":2,\"kind\":\"res\",\"op\":\"workspace.destroy\",\"payload\":$DESTROY_PAYLOAD}"

    exec 3<>"$fifo"
    nc -klU "$SOCK" < "$fifo" >> "$REQLOG" &
    STUB_NC_PID=$!

    ( tail -n +1 -F "$REQLOG" 2>/dev/null | while IFS= read -r line; do
        case "$(printf '%s' "$line" | jq -r '.op // empty' 2>/dev/null)" in
            hello) printf '%s\n' "$hello_reply" >&3 ;;
            workspace.list) printf '%s\n' "$list_reply" >&3 ;;
            workspace.destroy) printf '%s\n' "$destroy_reply" >&3 ;;
            version.query) printf '%s\n' "$version_reply" >&3 ;;
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
    LIST_PAYLOAD='{"workspaces":[]}'
    DESTROY_PAYLOAD='{"error":"refused","code":"test_refused"}'
}

# ws_entry ID SLUG LABEL — one workspace.list entry.
ws_entry() {
    jq -nc --arg id "$1" --arg slug "$2" --arg lbl "$3" \
        '{workspace_id:$id,slug:$slug,label:$lbl,project_root:"/nonexistent",runtime:"capsule",phase:"ready"}'
}

CASEN=0
CH=""
# new_home — a fresh comm home per case.
new_home() {
    CASEN=$((CASEN + 1))
    CH="$WORK/home-$CASEN"
    mkdir -p "$CH"
}

# seed_row NAME WSID — a registry row for NAME in the current comm home.
seed_row() {
    local name="$1" ws="$2" obj
    obj="$(jq -nc --arg ws "$ws" \
        '{host:"test-host",workspace_id:$ws,repo:"repo",root:"/nonexistent",expertise:[],status:"spawning",joined:"2026-09-30T00:00:00Z",last_seen:"2026-09-30T00:00:00Z"}')"
    SOT_COMM_HOME="$CH" bash -c 'source "$1"; ensure_home; with_lock registry_put "$2" "$3"' _ "$SCRIPTS_DIR/comm-lib.sh" "$name" "$obj"
}

reg_hash() { sha256sum "$CH/registry.json" 2>/dev/null | cut -d' ' -f1; }
reg_has() { jq -e --arg n "$1" '.agents | has($n)' "$CH/registry.json" >/dev/null 2>&1; }

run_despawn() {
    local who="$1"
    DESPAWN_OUT="$(env -u SOT_WORKSPACE -u SOT_WORKSPACE_ROOT -u SOT_RELAY_ENDPOINT -u SOT_SESSION \
        ${DESPAWN_PATH:+PATH="$DESPAWN_PATH"} XDG_CONFIG_HOME="$CH/xdg" \
        SOT_COMM_HOME="$CH" SOT_COMM_SELF_FILE="$CH/self.txt" \
        timeout 30 "$SCRIPTS_DIR/comm-despawn.sh" "$who" --endpoint "${DESPAWN_EP:-unix:$SOCK}" 2>"$CH/stderr.tmp")"
    DESPAWN_RC=$?
    DESPAWN_ERR="$(cat "$CH/stderr.tmp" 2>/dev/null || true)"
}

destroys() { grep -c '"op":"workspace.destroy"' "$REQLOG" 2>/dev/null || true; }

# Shared failure assertions: exit 1, the case's reason, registry untouched,
# no destroy sent.
assert_unresolved() {
    local who="$1" reason="$2" before="$3"
    [ "$DESPAWN_RC" -eq 1 ] || { echo "  exited $DESPAWN_RC (want 1): $DESPAWN_ERR"; return 1; }
    contains "$DESPAWN_ERR" "FAILED: comm-despawn could not resolve '$who' to a workspace" \
        || { echo "  stderr: $DESPAWN_ERR"; return 1; }
    contains "$DESPAWN_ERR" "$reason" || { echo "  reason missing ($reason): $DESPAWN_ERR"; return 1; }
    [ "$(reg_hash)" = "$before" ] || { echo "  the registry changed"; return 1; }
    [ "$(destroys)" -eq 0 ] || { echo "  a workspace.destroy was sent"; return 1; }
    return 0
}

case_row_without_workspace_id() {
    new_home; seed_row orphan ""
    local before; before="$(reg_hash)"
    start_stub_daemon; run_despawn orphan; stop_stub_daemon
    assert_unresolved orphan "its registry row records no workspace_id" "$before" || return 1
    contains "$DESPAWN_ERR" "comm-leave.sh --name orphan" || { echo "  no comm-leave hint: $DESPAWN_ERR"; return 1; }
}

# LOCAL_HOST is this box's declared host (sot_host), the fact a registry row's host carries: with the override
# unset, a hostname Test-Host is the declared test-host, so a row from "test-host" is this host's and a row from
# any other host is not.
case_local_host_rule_is_the_declared_host() {
    local bin="$WORK/fakehost-bin" saved="$SOT_SELF_HOST" name rc=0
    mkdir -p "$bin"
    unset SOT_SELF_HOST
    for name in Test-Host other-host; do
        printf '#!/bin/sh\nif [ "${1:-}" = "-s" ]; then echo %s; else echo %s; fi\n' "$name" "$name" > "$bin/hostname"
        chmod +x "$bin/hostname"
        new_home; seed_row orphan ""
        local before; before="$(reg_hash)"
        start_stub_daemon; DESPAWN_PATH="$bin:$PATH" run_despawn orphan; stop_stub_daemon
        assert_unresolved orphan "its registry row records no workspace_id" "$before" || rc=1
        if [ "$name" = Test-Host ]; then
            contains "$DESPAWN_ERR" "comm-leave.sh --name orphan" || { echo "  no comm-leave hint for $name: $DESPAWN_ERR"; rc=1; }
        else
            ! contains "$DESPAWN_ERR" "comm-leave" || { echo "  comm-leave hint for $name (a row of another host): $DESPAWN_ERR"; rc=1; }
        fi
    done
    export SOT_SELF_HOST="$saved"
    return "$rc"
}

# The destroyed row's identity slot is named by the TARGET daemon's declared host (version.query), which differs
# from this box's raw hostname and from its declared host here; only that exact slot goes, after the destroy.
seed_slots() {  # WSID DECLARED : the target's slot, a same-id slot of this box's host, another host's, another id's
    mkdir -p "$CH/self"
    printf 'target\n' > "$CH/self/$2__$1.txt"
    printf 'raw\n' > "$CH/self/test-host__$1.txt"
    printf 'elsewhere\n' > "$CH/self/other-box__$1.txt"
    printf 'sibling\n' > "$CH/self/$2__$1-fix.txt"
}
slots_survive() {  # WSID DECLARED
    [ -f "$CH/self/test-host__$1.txt" ] && [ -f "$CH/self/other-box__$1.txt" ] && [ -f "$CH/self/$2__$1-fix.txt" ]
}
queries_before_destroy() {
    local q d
    q="$(grep -n '"op":"version.query"' "$REQLOG" | head -n1 | cut -d: -f1)"; d="$(grep -n '"op":"workspace.destroy"' "$REQLOG" | head -n1 | cut -d: -f1)"
    [ -n "$q" ] && [ -n "$d" ] && [ "$q" -lt "$d" ]
}

case_declared_host_slot_with_no_registry_row() {
    new_home; seed_slots ws-e1 Remote-Box
    LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-e1 e1slug e1label)]}"
    DESTROY_PAYLOAD='{"workspace_id":"ws-e1"}'
    STUB_DECLARED_HOST="Remote-Box" start_stub_daemon; run_despawn e1slug; local rc=$DESPAWN_RC; queries_before_destroy; local order=$?; stop_stub_daemon
    [ "$rc" -eq 0 ] || { echo "  exited $rc: $DESPAWN_ERR"; return 1; }
    [ ! -e "$CH/self/Remote-Box__ws-e1.txt" ] || { echo "  the target's declared-host slot survived the destroy"; return 1; }
    slots_survive ws-e1 Remote-Box || { echo "  an unrelated slot was removed: $(ls "$CH/self" | tr '\n' ' ')"; return 1; }
    [ "$order" -eq 0 ] || { echo "  the host was not asked before the destroy"; return 1; }
}

case_declared_host_slot_with_a_registry_row() {
    local how
    for how in handle slug id; do
        new_home; seed_row hd-$how ws-e4; seed_slots ws-e4 Remote-Box
        LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-e4 e4slug e4label)]}"
        DESTROY_PAYLOAD='{"workspace_id":"ws-e4"}'
        STUB_DECLARED_HOST="Remote-Box" start_stub_daemon
        case "$how" in handle) run_despawn hd-handle ;; slug) run_despawn e4slug ;; id) run_despawn ws-e4 ;; esac
        local rc=$DESPAWN_RC; stop_stub_daemon
        [ "$rc" -eq 0 ] || { echo "  by $how: exited $rc: $DESPAWN_ERR"; return 1; }
        [ ! -e "$CH/self/Remote-Box__ws-e4.txt" ] || { echo "  by $how: the declared-host slot survived"; return 1; }
        slots_survive ws-e4 Remote-Box || { echo "  by $how: an unrelated slot was removed"; return 1; }
        if [ "$how" = handle ]; then ! reg_has hd-handle || { echo "  the registry row survived a confirmed destroy"; return 1; }; fi
    done
}

case_no_declared_host_or_an_unusable_one_destroys_nothing() {
    local host
    for host in "" "a/b" "$(printf 'a%.0s' $(seq 1 300))"; do
        new_home; seed_row hd-none ws-e2; seed_slots ws-e2 Remote-Box
        LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-e2 e2slug e2label)]}"
        DESTROY_PAYLOAD='{"workspace_id":"ws-e2"}'
        local before; before="$(reg_hash)"
        STUB_DECLARED_HOST="$host" start_stub_daemon; run_despawn hd-none; stop_stub_daemon
        [ "$DESPAWN_RC" -eq 1 ] || { echo "  host '${host:0:12}': exited $DESPAWN_RC (want 1): $DESPAWN_ERR"; return 1; }
        [ "$(destroys)" -eq 0 ] || { echo "  host '${host:0:12}': a workspace.destroy was sent"; return 1; }
        [ "$(reg_hash)" = "$before" ] || { echo "  host '${host:0:12}': the registry changed"; return 1; }
        [ -f "$CH/self/Remote-Box__ws-e2.txt" ] || { echo "  host '${host:0:12}': a slot was removed"; return 1; }
    done
}

case_a_refused_destroy_keeps_the_declared_host_slot() {
    new_home; seed_row hd-keep ws-e5; seed_slots ws-e5 Remote-Box
    LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-e5 e5slug e5label)]}"
    local before; before="$(reg_hash)"
    STUB_DECLARED_HOST="Remote-Box" start_stub_daemon; run_despawn hd-keep; stop_stub_daemon
    [ "$DESPAWN_RC" -eq 1 ] || { echo "  exited $DESPAWN_RC (want 1)"; return 1; }
    [ -f "$CH/self/Remote-Box__ws-e5.txt" ] && [ "$(reg_hash)" = "$before" ] || { echo "  a refused destroy changed the slot or the registry"; return 1; }
}

case_row_names_unlisted_workspace() {
    new_home; seed_row gone ws-gone
    local before; before="$(reg_hash)"
    start_stub_daemon; run_despawn gone; stop_stub_daemon
    assert_unresolved gone "names workspace 'ws-gone', which the daemon does not list" "$before"
}

case_no_row_no_workspace() {
    new_home; mkdir -p "$CH"
    SOT_COMM_HOME="$CH" bash -c 'source "$1"; ensure_home' _ "$SCRIPTS_DIR/comm-lib.sh"
    local before; before="$(reg_hash)"
    start_stub_daemon; run_despawn nobody; stop_stub_daemon
    assert_unresolved nobody "no registry row names it" "$before" || return 1
    ! contains "$DESPAWN_ERR" "comm-leave" || { echo "  unexpected comm-leave hint: $DESPAWN_ERR"; return 1; }
}

case_list_is_not_a_workspace_list() {
    new_home; seed_row h4 ws-4
    local before; before="$(reg_hash)"
    LIST_PAYLOAD='{"error":"x"}'
    start_stub_daemon; run_despawn h4; stop_stub_daemon
    assert_unresolved h4 "workspace.list returned no workspace list" "$before" || return 1
    ! contains "$DESPAWN_ERR" "comm-leave" || { echo "  comm-leave hint after a failed list: $DESPAWN_ERR"; return 1; }
}

case_refused_destroy_keeps_the_row() {
    new_home; seed_row h5 ws-5
    local before; before="$(reg_hash)"
    LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-5 other5 other5)]}"
    start_stub_daemon; run_despawn h5; stop_stub_daemon
    [ "$DESPAWN_RC" -eq 1 ] || { echo "  exited $DESPAWN_RC (want 1): $DESPAWN_ERR"; return 1; }
    contains "$DESPAWN_ERR" "ERROR: workspace.destroy failed" || { echo "  stderr: $DESPAWN_ERR"; return 1; }
    [ "$(reg_hash)" = "$before" ] || { echo "  the registry changed before a confirmed destroy"; return 1; }
}

# Regression guard for the reorder: success still deregisters.
case_confirmed_destroy_deregisters() {
    new_home; seed_row h6 ws-6
    LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-6 other6 other6)]}"
    DESTROY_PAYLOAD='{"workspace_id":"ws-6"}'
    start_stub_daemon; run_despawn h6
    local n; n="$(grep -c '"workspace_id":"ws-6"' "$REQLOG")"
    local d; d="$(destroys)"
    stop_stub_daemon
    [ "$DESPAWN_RC" -eq 0 ] || { echo "  exited $DESPAWN_RC: $DESPAWN_ERR"; return 1; }
    contains "$DESPAWN_OUT" "Destroyed workspace" || { echo "  stdout: $DESPAWN_OUT"; return 1; }
    contains "$DESPAWN_OUT" "Removed @h6" || { echo "  stdout: $DESPAWN_OUT"; return 1; }
    ! reg_has h6 || { echo "  registry row survived a confirmed destroy"; return 1; }
    [ "$d" -eq 1 ] && [ "$n" -ge 1 ] || { echo "  destroys=$d ws-6 mentions=$n"; return 1; }
}

# A Windows box's daemon listens only on a named pipe: the request goes
# through the stand-in bridge (stubbed by a script that carries stdin to the stub
# daemon's socket and its replies back).
case_confirmed_destroy_over_pipe_endpoint() {
    new_home; seed_row h9 ws-9
    LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-9 other9 other9)]}"
    DESTROY_PAYLOAD='{"workspace_id":"ws-9"}'
    start_stub_daemon
    mkdir -p "$WORK/bin"
    GUARD_PIPE_SOCKET="$SOCK" DESPAWN_EP='pipe:\\.\pipe\sot-stub' DESPAWN_PATH="$WORK/bin:$PATH" run_despawn h9
    stop_stub_daemon
    [ "$DESPAWN_RC" -eq 0 ] || { echo "  exited $DESPAWN_RC: $DESPAWN_ERR"; return 1; }
    contains "$DESPAWN_OUT" "Destroyed workspace" || { echo "  stdout: $DESPAWN_OUT"; return 1; }
    ! reg_has h9 || { echo "  registry row survived a confirmed destroy"; return 1; }
}

# Only the destroyed workspace's own identity slot goes, by its exact name.
case_self_file_exact_name() {
    new_home; seed_row proj-wt-b3 ws-b3; seed_row proj-wt-b3-fix ws-b3-fix
    mkdir -p "$CH/self"
    printf 'mine\n' > "$CH/self/test-host__ws-b3.txt"
    printf 'sibling\n' > "$CH/self/test-host__ws-b3-fix.txt"
    printf 'main\n' > "$CH/self/test-host__ws-main.txt"
    local sib main; sib="$(sha256sum "$CH/self/test-host__ws-b3-fix.txt")"; main="$(sha256sum "$CH/self/test-host__ws-main.txt")"
    LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-b3 s1 l1),$(ws_entry ws-b3-fix s2 l2)]}"
    DESTROY_PAYLOAD='{"workspace_id":"ws-b3"}'
    start_stub_daemon; run_despawn proj-wt-b3; stop_stub_daemon
    [ "$DESPAWN_RC" -eq 0 ] || { echo "  exited $DESPAWN_RC: $DESPAWN_ERR"; return 1; }
    [ ! -e "$CH/self/test-host__ws-b3.txt" ] || { echo "  its own self-file survived"; return 1; }
    [ "$(sha256sum "$CH/self/test-host__ws-b3-fix.txt")" = "$sib" ] || { echo "  the sibling's self-file changed"; return 1; }
    [ "$(sha256sum "$CH/self/test-host__ws-main.txt")" = "$main" ] || { echo "  the main self-file changed"; return 1; }
}

# A handle equal to ANOTHER workspace's label destroys its own recorded one.
case_recorded_workspace_first() {
    new_home; seed_row h8 ws-8
    LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-other s9 h8),$(ws_entry ws-8 s8 l8)]}"
    DESTROY_PAYLOAD='{"workspace_id":"ws-8"}'
    start_stub_daemon; run_despawn h8
    local mine other
    mine="$(grep -c '"op":"workspace.destroy".*"workspace_id":"ws-8"' "$REQLOG")"
    other="$(grep -c '"op":"workspace.destroy".*"workspace_id":"ws-other"' "$REQLOG")"
    stop_stub_daemon
    [ "$DESPAWN_RC" -eq 0 ] || { echo "  exited $DESPAWN_RC: $DESPAWN_ERR"; return 1; }
    [ "$mine" -eq 1 ] && [ "$other" -eq 0 ] || { echo "  destroyed own=$mine other=$other"; return 1; }
}

# --- comm-worktree-clean.sh ----------------------------------------------

# make_worktree — proj repo with a worktree proj-wt-x and a display prefix.
make_worktree() {
    git init -q -b main "$WORK/wt/proj"
    git -C "$WORK/wt/proj" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
    git -C "$WORK/wt/proj" worktree add -q -b wt/x "$WORK/wt/worktrees/proj-wt-x"
    mkdir -p "$WORK/wt/proj/.sot"
    printf 'display_prefix = ".P"\n' > "$WORK/wt/proj/.sot/worktree.toml"
}

run_clean() {  # [flag] — --force unless given ("" for none)
    local opt="${1---force}"
    CLEAN_OUT="$(cd "$WORK/wt/proj" && env -u SOT_WORKSPACE -u SOT_WORKSPACE_ROOT -u SOT_RELAY_ENDPOINT -u SOT_SESSION -u SOT_SPAWN_ENDPOINT \
        SOT_SPAWN_ENDPOINT="unix:$SOCK" XDG_CONFIG_HOME="$CH/xdg" \
        SOT_COMM_HOME="$CH" SOT_COMM_SELF_FILE="$CH/self.txt" \
        timeout 60 "$SCRIPTS_DIR/comm-worktree-clean.sh" x $opt 2>&1)"
    CLEAN_RC=$?
}

lists() { grep -c '"op":"workspace.list"' "$REQLOG" 2>/dev/null || true; }

case_worktree_clean_despawns_once() {
    rm -rf "${WORK:?}/wt"; make_worktree
    new_home; seed_row proj-wt-x ws-wt
    LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-wt p-wt-x .P-wt-x)]}"
    DESTROY_PAYLOAD='{"workspace_id":"ws-wt"}'
    start_stub_daemon; run_clean
    local l; l="$(lists)"
    stop_stub_daemon
    [ "$CLEAN_RC" -eq 0 ] || { echo "  exited $CLEAN_RC: $CLEAN_OUT"; return 1; }
    contains "$CLEAN_OUT" "Destroyed workspace" || { echo "  out: $CLEAN_OUT"; return 1; }
    contains "$CLEAN_OUT" "session @proj-wt-x despawned" || { echo "  out: $CLEAN_OUT"; return 1; }
    ! contains "$CLEAN_OUT" "FAILED" || { echo "  out: $CLEAN_OUT"; return 1; }
    [ "$l" -eq 1 ] || { echo "  workspace.list seen $l times (want 1)"; return 1; }
}

# Guard: with no registry row the LABEL fallback still destroys.
case_worktree_clean_label_fallback() {
    rm -rf "${WORK:?}/wt"; make_worktree
    new_home; mkdir -p "$CH"
    LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-wt p-wt-x .P-wt-x)]}"
    DESTROY_PAYLOAD='{"workspace_id":"ws-wt"}'
    start_stub_daemon; run_clean
    local d; d="$(destroys)"
    stop_stub_daemon
    [ "$CLEAN_RC" -eq 0 ] || { echo "  exited $CLEAN_RC: $CLEAN_OUT"; return 1; }
    contains "$CLEAN_OUT" "Destroyed workspace" || { echo "  out: $CLEAN_OUT"; return 1; }
    contains "$CLEAN_OUT" "session @proj-wt-x despawned" || { echo "  out: $CLEAN_OUT"; return 1; }
    [ "$d" -eq 1 ] || { echo "  destroys=$d (want 1)"; return 1; }
}

# Handle row has a stale workspace id and the label differs: the label pass
# destroys the workspace and the handle's row goes too.
case_worktree_clean_label_deregisters() {
    rm -rf "${WORK:?}/wt"; make_worktree
    new_home; seed_row proj-wt-x ws-stale
    LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-wt p-wt-x .P-wt-x)]}"
    DESTROY_PAYLOAD='{"workspace_id":"ws-wt"}'
    start_stub_daemon; run_clean
    local d; d="$(destroys)"
    stop_stub_daemon
    [ "$CLEAN_RC" -eq 0 ] || { echo "  exited $CLEAN_RC: $CLEAN_OUT"; return 1; }
    [ "$d" -eq 1 ] || { echo "  destroys=$d (want 1)"; return 1; }
    ! reg_has proj-wt-x || { echo "  @proj-wt-x is still registered"; return 1; }
}

# A despawn that fails leaves the worktree in place.
case_worktree_clean_keeps_on_failed_despawn() {
    rm -rf "${WORK:?}/wt"; make_worktree
    new_home; seed_row proj-wt-x ws-wt
    LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-wt p-wt-x .P-wt-x)]}"
    start_stub_daemon; run_clean
    stop_stub_daemon
    [ "$CLEAN_RC" -ne 0 ] || { echo "  exited 0: $CLEAN_OUT"; return 1; }
    [ -d "$WORK/wt/worktrees/proj-wt-x" ] || { echo "  the worktree was removed"; return 1; }
    contains "$CLEAN_OUT" "--keep-session" || { echo "  the refusal names no --keep-session: $CLEAN_OUT"; return 1; }
}

# Without --force a dirty worktree is refused BEFORE the session is destroyed.
case_worktree_clean_dirty_keeps_the_session() {
    rm -rf "${WORK:?}/wt"; make_worktree
    : > "$WORK/wt/worktrees/proj-wt-x/untracked.txt"
    new_home; seed_row proj-wt-x ws-wt
    LIST_PAYLOAD="{\"workspaces\":[$(ws_entry ws-wt p-wt-x .P-wt-x)]}"
    DESTROY_PAYLOAD='{"workspace_id":"ws-wt"}'
    start_stub_daemon; run_clean ""
    local d; d="$(destroys)"
    stop_stub_daemon
    [ "$CLEAN_RC" -ne 0 ] || { echo "  exited 0: $CLEAN_OUT"; return 1; }
    [ "$d" -eq 0 ] || { echo "  destroys=$d (want 0): the session went before the refusal"; return 1; }
    [ -d "$WORK/wt/worktrees/proj-wt-x" ] || { echo "  the worktree was removed"; return 1; }
    reg_has proj-wt-x || { echo "  @proj-wt-x was deregistered"; return 1; }
}

check "D1 registry row without a workspace_id: refused, row kept, comm-leave hint" case_row_without_workspace_id
check "D1b LOCAL_HOST is the declared host: the comm-leave hint follows it" case_local_host_rule_is_the_declared_host
check "D10 the target's declared-host slot goes after a confirmed destroy, with no registry row" case_declared_host_slot_with_no_registry_row
check "D11 the same with a registry row, by handle, slug and id" case_declared_host_slot_with_a_registry_row
check "D12 no declared host, or one that cannot name a slot, destroys nothing" case_no_declared_host_or_an_unusable_one_destroys_nothing
check "D13 a refused destroy keeps the slot and the row" case_a_refused_destroy_keeps_the_declared_host_slot
check "D2 registry row names an unlisted workspace: refused, row kept" case_row_names_unlisted_workspace
check "D3 no row and no workspace: refused, no comm-leave hint" case_no_row_no_workspace
check "D4 workspace.list is not a workspace list: refused, row kept, no comm-leave hint" case_list_is_not_a_workspace_list
check "D5 destroy refused: exit 1, row kept" case_refused_destroy_keeps_the_row
check "D6 confirmed destroy: row removed afterwards" case_confirmed_destroy_deregisters
check "D7 despawn removes only its own self-file, by exact name" case_self_file_exact_name
check "D8 despawn destroys the registry's recorded workspace first" case_recorded_workspace_first
check "D9 pipe: endpoint (Windows local daemon): destroy succeeds through the stand-in bridge" case_confirmed_destroy_over_pipe_endpoint
check "W1 worktree-clean despawns once, by handle" case_worktree_clean_despawns_once
check "W2 worktree-clean falls back to the label with no registry row" case_worktree_clean_label_fallback
check "W3 label fallback also deregisters the handle" case_worktree_clean_label_deregisters
check "W4 failed despawn keeps the worktree, exits nonzero, names --keep-session" case_worktree_clean_keeps_on_failed_despawn
check "W5 a dirty worktree without --force is refused before the session is destroyed" case_worktree_clean_dirty_keeps_the_session

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
