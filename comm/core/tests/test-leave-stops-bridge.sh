#!/usr/bin/env bash
# test-leave-stops-bridge.sh — self-contained test for ADR 0043 decision 35:
# "The default row's end prunes its registry row as destroy does;
# comm-leave.sh stops its handle's bridge (comm-listen.sh --stop); the
# watcher dies with the agent." This suite covers the shell half — the SELF
# branch of comm-leave.sh (removing YOUR OWN row) now also stops the
# `commbridge-<handle>` tmux session for that handle before dropping its
# registry row, so the reconnect-loop bridge doesn't outlive the handle
# that owns it (nothing else did today). The `--name <other>` branch is
# untouched by design (comm-despawn.sh is the full-teardown tool for
# someone else's row) — case_leave_by_name_never_stops_the_other_handles_bridge
# below guards that.
#
# No bats dependency. HERMETIC, same seams as test-join-disambiguation.sh:
# a temp $SOT_COMM_HOME, a per-case $SOT_COMM_SELF_FILE, a pinned
# $SOT_COMM_TEST_HOST, and an isolated tmux server ($SOT_TMUX_SOCK) — never
# touches the real ~/.sot-comm or the real per-user tmux socket. The fake
# bridge sessions are plain `sleep 60` placeholders (comm-listen.sh --stop's
# kill only checks the session exists under the exact `commbridge-<handle>`
# name, same technique
# test-join-disambiguation.sh:case_join_warns_on_stranding_escalation_when_bridge_running
# uses to mock a bridge marker).
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

# Isolated tmux server (security review's own rule -- see comm-lib.sh's
# sot_tmux_socket doc: a caller-set $SOT_TMUX_SOCK is honoured as-is) —
# every fake bridge session AND every comm-listen.sh --stop this suite
# triggers lands here, never the real per-user socket.
export SOT_TMUX_SOCK="$WORK/tmux.sock"
trap 'tmux -S "$SOT_TMUX_SOCK" kill-server >/dev/null 2>&1 || true; rm -rf "$WORK"' EXIT

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

bridge_exists() {  # HANDLE -> 0 if the exact commbridge-<HANDLE> session exists
    tmux -S "$SOT_TMUX_SOCK" has-session -t "=commbridge-$1" 2>/dev/null
}

start_fake_bridge() {  # HANDLE -> start a "sleep 60" placeholder session
    tmux -S "$SOT_TMUX_SOCK" new-session -d -s "commbridge-$1" "sleep 60" 2>/dev/null
}

case_leave_stops_own_bridge_prunes_row_and_spares_a_sibling_session() {
    if ! command -v tmux >/dev/null 2>&1; then
        echo "  SKIP: no tmux on this host -- cannot mock a bridge session"
        return 2
    fi
    local handle="t-leave-$$" self out rc
    self="$WORK/self-own.txt"

    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
        "$JOIN" --name "$handle" 2>&1)"
    rc=$?
    [ "$rc" -eq 0 ] || { echo "  comm-join.sh --name $handle exited $rc: $out"; return 1; }
    contains "$out" "Joined sot-comm as @$handle" \
        || { echo "  join did not confirm @$handle: $out"; return 1; }

    start_fake_bridge "$handle" \
        || { echo "  SKIP: could not create the mock bridge session on the isolated socket"; return 2; }
    # A sibling whose name merely starts with this handle's bridge name --
    # tmux's target grammar falls back to prefix/glob matching without a
    # leading '=', so an unanchored kill would take this out too.
    start_fake_bridge "$handle-sibling" \
        || { echo "  SKIP: could not create the sibling mock bridge session"; return 2; }

    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" "$LEAVE" 2>&1)"
    rc=$?
    [ "$rc" -eq 0 ] || { echo "  comm-leave.sh exited $rc: $out"; return 1; }
    contains "$out" "Left sot-comm (@$handle removed)" \
        || { echo "  leave did not confirm @$handle removed: $out"; return 1; }

    if bridge_exists "$handle"; then
        echo "  FAIL: commbridge-$handle is still running after comm-leave.sh"
        return 1
    fi
    if ! bridge_exists "$handle-sibling"; then
        echo "  FAIL: the sibling commbridge-$handle-sibling was killed too (exact-name match broke)"
        return 1
    fi
    if registry_has_handle "$handle"; then
        echo "  FAIL: @$handle still has a registry row after comm-leave.sh"
        return 1
    fi
    return 0
}

case_leave_by_name_never_stops_the_other_handles_bridge() {
    if ! command -v tmux >/dev/null 2>&1; then
        echo "  SKIP: no tmux on this host -- cannot mock a bridge session"
        return 2
    fi
    local other="t-leave-other-$$" self_other out rc
    self_other="$WORK/self-other.txt"

    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$self_other" SOT_COMM_TEST_HOST="$HOST" \
        "$JOIN" --name "$other" 2>&1)"
    rc=$?
    [ "$rc" -eq 0 ] || { echo "  comm-join.sh --name $other exited $rc: $out"; return 1; }

    start_fake_bridge "$other" \
        || { echo "  SKIP: could not create the mock bridge session on the isolated socket"; return 2; }

    # A DIFFERENT (unjoined) caller removes @$other's row by name --
    # comm-despawn.sh, not this path, owns tearing down someone else's
    # bridge (comm-leave.sh's own :22-30 branch, untouched by this lane).
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
    if ! bridge_exists "$other"; then
        echo "  FAIL: commbridge-$other was stopped by a --name removal of someone else's row"
        return 1
    fi
    return 0
}

contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

check "comm-leave.sh (self) stops its own bridge, prunes its row, spares a same-prefix sibling session" \
    case_leave_stops_own_bridge_prunes_row_and_spares_a_sibling_session
check "comm-leave.sh --name <other> removes only the row -- the other handle's bridge survives" \
    case_leave_by_name_never_stops_the_other_handles_bridge

echo ""
echo "$PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
