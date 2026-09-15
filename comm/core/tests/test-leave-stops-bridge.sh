#!/usr/bin/env bash
# test-leave-stops-bridge.sh — self-contained test for ADR 0043 decision 35:
# "The default row's end prunes its registry row as destroy does;
# comm-leave.sh stops its handle's bridge (comm-listen.sh --stop); the
# watcher dies with the agent." This suite covers the shell half — the SELF
# branch of comm-leave.sh (removing YOUR OWN row) also stops the handle's
# relay bridge (the loop comm-lib.sh's sot_bridge_start recorded in a
# pidfile) before dropping its registry row, so the bridge doesn't outlive
# the handle that owns it. The `--name <other>` branch is untouched by
# design (comm-despawn.sh is the full-teardown tool for someone else's row)
# — case_leave_by_name_never_stops_the_other_handles_bridge guards that.
#
# No bats dependency. HERMETIC, same seams as test-join-disambiguation.sh:
# a temp $SOT_COMM_HOME (pidfiles live under it), a per-case
# $SOT_COMM_SELF_FILE, and a pinned $SOT_COMM_TEST_HOST — never touches the
# real ~/.sot-comm. The bridges are real sot_bridge_start loops running a
# fake relay (`sleep 60`) in place of comm-relay.sh, so the pidfile + argv
# identification is exercised for real.
#
# Usage: comm/core/tests/test-leave-stops-bridge.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
JOIN="$SCRIPTS_DIR/comm-join.sh"
LEAVE="$SCRIPTS_DIR/comm-leave.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-leave-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi

export SOT_COMM_HOME="$WORK/home"
mkdir -p "$SOT_COMM_HOME"
REGISTRY="$SOT_COMM_HOME/registry.json"
source "$SCRIPTS_DIR/comm-lib.sh"
ensure_home
unset SOT_WORKSPACE_ID

# Named comm-relay.sh so sot_bridge_stop's process pattern reaps it too.
mkdir -p "$WORK/fakebin"; FAKE_RELAY="$WORK/fakebin/comm-relay.sh"
printf '#!/bin/sh\nsleep 60\n' > "$FAKE_RELAY"; chmod +x "$FAKE_RELAY"
STARTED=()
cleanup() {
    local h
    for h in "${STARTED[@]}"; do sot_bridge_stop "$h" 2>/dev/null; done
    rm -rf "$WORK"
}
trap cleanup EXIT

HOST="testhost"
export SOT_COMM_TEST_HOST="$HOST"

PASS=0
FAIL=0
SKIP=0

# Same tri-state runner as test-join-disambiguation.sh's check(): SKIP is a
# distinct outcome from PASS (an unexecuted guard must never count as
# verified).
check() {
    local desc="$1" fn="$2"
    local rc
    "$fn"
    rc=$?
    case "$rc" in
        0) echo "PASS: $desc"; PASS=$((PASS + 1)) ;;
        2) echo "SKIP: $desc"; SKIP=$((SKIP + 1)) ;;
        *) echo "FAIL: $desc"; FAIL=$((FAIL + 1)) ;;
    esac
}

registry_has_handle() {  # NAME -> 0 if present, 1 otherwise
    jq -e --arg n "$1" '.agents | has($n)' "$REGISTRY" >/dev/null 2>&1
}

start_fake_bridge() {  # HANDLE -> a real recorded loop around the fake relay
    STARTED+=("$1")
    sot_bridge_start "$1" "$FAKE_RELAY"
    sot_bridge_running_for "$1"
}

bridge_gone() {  # HANDLE -> 0 once neither the loop nor any bridge process for it is alive
    local tries=0
    while [ "$tries" -lt 50 ]; do
        ! sot_bridge_running_for "$1" && [ -z "$(sot_bridge_pids_for "$1")" ] && return 0
        sleep 0.1; tries=$((tries + 1))
    done
    return 1
}

case_leave_stops_own_bridge_prunes_row_and_spares_a_sibling_bridge() {
    local handle="t-leave-$$" self out rc
    self="$WORK/self-own.txt"

    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
        "$JOIN" --name "$handle" 2>&1)"
    rc=$?
    [ "$rc" -eq 0 ] || { echo "  comm-join.sh --name $handle exited $rc: $out"; return 1; }
    contains "$out" "Joined sot-comm as @$handle" \
        || { echo "  join did not confirm @$handle: $out"; return 1; }

    start_fake_bridge "$handle" || { echo "  setup: could not start the bridge for @$handle"; return 1; }
    # A sibling whose handle merely starts with this one: an unanchored
    # process match would take it out too.
    start_fake_bridge "$handle-sibling" || { echo "  setup: could not start the sibling bridge"; return 1; }

    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" "$LEAVE" 2>&1)"
    rc=$?
    [ "$rc" -eq 0 ] || { echo "  comm-leave.sh exited $rc: $out"; return 1; }
    contains "$out" "Left sot-comm (@$handle removed)" \
        || { echo "  leave did not confirm @$handle removed: $out"; return 1; }

    bridge_gone "$handle" || { echo "  FAIL: the bridge for @$handle is still running after comm-leave.sh"; return 1; }
    [ ! -f "$(sot_bridge_pidfile "$handle")" ] || { echo "  FAIL: the pidfile for @$handle survived comm-leave.sh"; return 1; }
    sot_bridge_running_for "$handle-sibling" \
        || { echo "  FAIL: the sibling bridge for @$handle-sibling was killed too (exact match broke)"; return 1; }
    if registry_has_handle "$handle"; then
        echo "  FAIL: @$handle still has a registry row after comm-leave.sh"
        return 1
    fi
    return 0
}

case_leave_by_name_never_stops_the_other_handles_bridge() {
    local other="t-leave-other-$$" self_other out rc
    self_other="$WORK/self-other.txt"

    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$self_other" SOT_COMM_TEST_HOST="$HOST" \
        "$JOIN" --name "$other" 2>&1)"
    rc=$?
    [ "$rc" -eq 0 ] || { echo "  comm-join.sh --name $other exited $rc: $out"; return 1; }

    start_fake_bridge "$other" || { echo "  setup: could not start the bridge for @$other"; return 1; }

    # A DIFFERENT (unjoined) caller removes @$other's row by name --
    # comm-despawn.sh, not this path, owns tearing down someone else's
    # bridge (comm-leave.sh's own --name branch, untouched by this lane).
    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-caller.txt" SOT_COMM_TEST_HOST="$HOST" \
        "$LEAVE" --name "$other" 2>&1)"
    rc=$?
    [ "$rc" -eq 0 ] || { echo "  comm-leave.sh --name $other exited $rc: $out"; return 1; }
    contains "$out" "Removed @$other from the registry" \
        || { echo "  leave did not confirm @$other's row removed: $out"; return 1; }

    if registry_has_handle "$other"; then
        echo "  FAIL: @$other still has a registry row after comm-leave.sh --name $other"
        return 1
    fi
    sot_bridge_running_for "$other" \
        || { echo "  FAIL: the bridge for @$other was stopped by a --name removal of someone else's row"; return 1; }
    return 0
}

contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

check "comm-leave.sh (self) stops its own bridge, prunes its row, spares a same-prefix sibling bridge" \
    case_leave_stops_own_bridge_prunes_row_and_spares_a_sibling_bridge
check "comm-leave.sh --name <other> removes only the row -- the other handle's bridge survives" \
    case_leave_by_name_never_stops_the_other_handles_bridge

echo ""
echo "$PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
