#!/usr/bin/env bash
# test-session-start-survived.sh — hermetic regression suite for the
# bootstrap's survival check (comm-session-start.sh --context): a watcher
# counts as SURVIVED only when it is alive AND was armed by THIS session.
# Field history (2026-09-06 / 09-08 / 09-10, three boxes): an orphan watcher
# from a dead session passed a liveness-only check and left the new session
# deaf while the bootstrap said "nothing to do".
#
# Since 2026-09-20 it also covers the OTHER half: a survived watcher says
# nothing about the relay bridge, and the block must never tell a session
# with a dead bridge not to re-listen (a peer session sat deaf for half an
# hour on exactly that sentence).
#
# Runs against a temp $SOT_COMM_HOME with a pinned self-file — never touches
# the real ~/.sot-comm. Usage: comm/core/tests/test-session-start-survived.sh
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-survived-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SLEEPERS=()
# This suite leaked a fake watcher per case -- twenty were found alive on the
# hub across four of its temp directories. TWO causes, and the first is why
# the SLEEPERS array alone could never have worked: `sleeper` is called in a
# command substitution, so its `SLEEPERS+=` happens in a subshell and this
# shell's array stays empty. The sweep therefore goes by this suite's OWN temp
# path, which cannot match anything else on the box -- and by a PREFIX, so it
# covers both fakebin stand-ins (comm-watch.sh and comm-wake.sh) rather than
# whichever one it was written for. The second cause is that
# the fake watcher is a shell running `sleep 300`, so killing the shell leaves
# the sleep behind: children first, then the shell.
cleanup() {
    sot_bridge_stop "${NAME:-}" 2>/dev/null || true
    # AND the bridge fake's own processes, which sot_bridge_stop cannot reach.
    # It reaps by `_sot_bridge_pattern`, which matches `comm-relay.sh ... bridge
    # --name <handle>` or `sot-bridge <relay> <handle>` -- and this suite's fake
    # is deliberately named fake-relay.sh, matching NEITHER. That is load-bearing
    # and must stay: it is what makes a leftover here INERT, unable to be read as
    # a live bridge by the tether case. The cost of that choice is that the
    # suite has to reap its own, which it never did -- six per run, one per
    # bridge start, outliving their temp directory. Never rename this fake to
    # comm-relay.sh to "fix" it; that trades a leak for a suite that can pass
    # on a dead bridge.
    for p in $(pgrep -f "$WORK/fake-relay.sh" 2>/dev/null); do
        pkill -P "$p" 2>/dev/null || true
        kill "$p" 2>/dev/null || true
    done
    local p
    for p in $(pgrep -f "$WORK/fakebin/comm-w" 2>/dev/null) "${SLEEPERS[@]:-}"; do
        [ -n "$p" ] || continue
        pkill -P "$p" 2>/dev/null || true
        kill "$p" 2>/dev/null || true
    done
    rm -rf "${WORK:?}"
}
trap cleanup EXIT

export SOT_COMM_SELF_FILE="$WORK/self.txt"
export SOT_COMM_TEST_HOST="testhost"
unset SOT_COMM_NAME
mkdir -p "$SOT_COMM_HOME/state"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
# shellcheck source=../scripts/comm-lib.sh
source "$SCRIPTS_DIR/comm-lib.sh"
ensure_home
NAME="survived-test"
eval "$("$SCRIPTS_DIR/comm-context.sh" 2>/dev/null | grep -E '^(REPO|PROJECT_ROOT)=')"
sot_write_self_file "$SOT_COMM_SELF_FILE" "$NAME" "$REPO" "$PROJECT_ROOT" || { echo "self-file write failed" >&2; exit 1; }
[ "$("$SCRIPTS_DIR/comm-context.sh" | sed -n 's/^NAME=//p')" = "$NAME" ] || { echo "context did not resolve the pinned self-file" >&2; exit 1; }
# The registry must attribute the handle to OUR project root (_owns_handle).
jq -n --arg n "$NAME" --arg r "$PROJECT_ROOT" '{agents:{($n):{state:"idle", root:$r, repo:"x"}}}' > "$REGISTRY"
MARKER="$SOT_COMM_HOME/state/$NAME.watch"

# A planted watcher has to be identifiable AS one: _survived checks the marker
# pid's command line names a watcher script and this handle, because the marker
# outlives reboots on a shared home and a reused pid would otherwise report
# SURVIVED for a session with no watcher at all.
#
# TWO FAKES, and which one a case plants is the point rather than a detail.
# `wake_sleeper` is a comm-wake.sh — the PING watcher, which is what survival
# MEANS now: a Monitor is a wake path nobody re-arms, so it cannot stand for
# one here. `sleeper` is a comm-watch.sh Monitor, kept for the cases that are
# ABOUT a Monitor, where it must read as NOT survived and come through
# unreaped.
PLANTED="$WORK/planted"; : > "$PLANTED"
mkdir -p "$WORK/fakebin"
FAKE_WATCHER="$WORK/fakebin/comm-watch.sh"
printf '#!/bin/sh\nsleep 300\n' > "$FAKE_WATCHER"; chmod +x "$FAKE_WATCHER"
# The pid goes to a FILE, not the array: these are called in a command
# substitution, so `SLEEPERS+=` would happen in a subshell and this shell's
# array would stay empty -- the reason this suite leaked a watcher per case
# for as long as it has existed.
sleeper() { "$FAKE_WATCHER" "$NAME" >/dev/null 2>&1 & local p=$!; printf '%s\n' "$p" >> "$PLANTED"; echo "$p"; }
# A PING watcher stand-in, for the cases that need survival to short-circuit
# the bootstrap: only a live comm-wake does that now, because a Monitor is a
# wake path nobody re-arms (see comm-session-start.sh's survived branch).
FAKE_WAKE="$WORK/fakebin/comm-wake.sh"
printf '#!/bin/sh\nsleep 300\n' > "$FAKE_WAKE"; chmod +x "$FAKE_WAKE"
wake_sleeper() { "$FAKE_WAKE" "$NAME" >/dev/null 2>&1 & local p=$!; printf '%s\n' "$p" >> "$PLANTED"; echo "$p"; }
ctx() { CLAUDE_CODE_SESSION_ID="$1" bash "$SCRIPTS_DIR/comm-session-start.sh" --context 2>"$WORK/err" | head -n1; }

PASS=0; FAIL=0
check() {
    local d="$1"; shift
    if "$@"; then PASS=$((PASS+1)); echo "PASS $d"; else FAIL=$((FAIL+1)); echo "FAIL $d"; fi
    # EVERY CASE STARTS FROM NOTHING RUNNING. Several cases assert the
    # ABSENCE of a ping watcher for this handle -- the bootstrap's own
    # did-it-come-up check is exactly that question -- so a fixture or a real
    # watcher left behind by an earlier case does not merely leak, it makes
    # the next case pass on the wrong process. Both sweeps are scoped: the
    # planted pids are this suite's own, and the pattern carries this suite's
    # handle, which no real session shares.
    local p
    while read -r p; do
        [ -n "$p" ] || continue
        pkill -P "$p" 2>/dev/null || true
        kill "$p" 2>/dev/null || true
    done < "$PLANTED"
    : > "$PLANTED"
    for p in $(pgrep -f "comm-[w]ake.sh $NAME" 2>/dev/null); do kill "$p" 2>/dev/null; done
}

case_own_live_watcher_survives() {
    local p; p="$(wake_sleeper)"; printf '%s\nsess-A\n' "$p" > "$MARKER"
    [[ "$(ctx sess-A)" == "SURVIVED handle=$NAME listener="* ]] || { echo "    got '$(ctx sess-A)'"; return 1; }
    kill -0 "$p" 2>/dev/null || { echo "    own watcher was killed"; return 1; }
}
case_orphan_from_another_session_is_not_survived_and_reaped() {
    local p; p="$(wake_sleeper)"; printf '%s\nsess-OLD\n' "$p" > "$MARKER"
    local out; out="$(ctx sess-NEW)"
    [[ "$out" == "NOT SURVIVED handle=$NAME"* ]] || { echo "    got '$out'"; return 1; }
    grep -q "ORPHAN watcher pid=$p" "$WORK/err" || { echo "    no reap notice: $(cat "$WORK/err")"; return 1; }
    sleep 0.3; ! kill -0 "$p" 2>/dev/null || { echo "    orphan still alive"; return 1; }
}
case_legacy_marker_without_session_line_keeps_liveness_answer() {
    local p; p="$(wake_sleeper)"; printf '%s' "$p" > "$MARKER"
    [[ "$(ctx sess-B)" == "SURVIVED handle=$NAME listener="* ]] || { echo "    got '$(ctx sess-B)'"; return 1; }
}
case_dead_pid_is_not_survived() {
    local p; p="$(wake_sleeper)"; kill "$p"; wait "$p" 2>/dev/null; printf '%s\nsess-A\n' "$p" > "$MARKER"
    [[ "$(ctx sess-A)" == "NOT SURVIVED handle=$NAME"* ]] || { echo "    got '$(ctx sess-A)'"; return 1; }
}
case_no_session_id_in_env_trusts_liveness() {
    local p; p="$(wake_sleeper)"; printf '%s\nsess-OLD\n' "$p" > "$MARKER"
    local out; out="$(env -u CLAUDE_CODE_SESSION_ID bash "$SCRIPTS_DIR/comm-session-start.sh" --context 2>/dev/null | head -n1)"
    [[ "$out" == "SURVIVED handle=$NAME listener="* ]] || { echo "    got '$out'"; return 1; }
}

# --- the bridge half (2026-09-20) -------------------------------------
# A bridge-shaped loop the real predicate accepts (argv[3]=sot-bridge,
# argv[5]=handle), pointed at a relay that just sleeps: no daemon needed.
printf '#!/usr/bin/env bash\nsleep 300\n' > "$WORK/fake-relay.sh"; chmod +x "$WORK/fake-relay.sh"
full() { CLAUDE_CODE_SESSION_ID="$1" bash "$SCRIPTS_DIR/comm-session-start.sh" --context 2>"$WORK/err"; }

case_live_bridge_is_reported_up_and_keeps_the_do_not_relisten_line() {
    local p; p="$(wake_sleeper)"; printf '%s\nsess-A\n' "$p" > "$MARKER"
    sot_bridge_start "$NAME" "$WORK/fake-relay.sh"
    sot_bridge_running_for "$NAME" || { echo "    fixture bridge did not start"; return 1; }
    local out; out="$(full sess-A)"
    [[ "$(printf '%s' "$out" | head -n1)" == "SURVIVED handle=$NAME listener=up" ]] \
        || { echo "    got '$(printf '%s' "$out" | head -n1)'"; return 1; }
    printf '%s' "$out" | grep -q "Do not re-join, re-listen, or re-poll" \
        || { echo "    a live bridge must keep the do-not-re-listen line"; return 1; }
    sot_bridge_stop "$NAME"
}
case_dead_bridge_is_reported_down_and_never_says_do_not_relisten() {
    local p; p="$(wake_sleeper)"; printf '%s\nsess-A\n' "$p" > "$MARKER"
    sot_bridge_stop "$NAME" 2>/dev/null || true
    local out; out="$(full sess-A)"
    [[ "$(printf '%s' "$out" | head -n1)" == "SURVIVED handle=$NAME listener=down" ]] \
        || { echo "    got '$(printf '%s' "$out" | head -n1)'"; return 1; }
    printf '%s' "$out" | grep -q "re-listen" \
        && { echo "    told a deaf session not to re-listen"; return 1; }
    printf '%s' "$out" | grep -q "comm-listen.sh --name $NAME now" \
        || { echo "    no handle-explicit instruction to restart the listener"; return 1; }
    ! sot_bridge_running_for "$NAME" || { echo "    the query path started a bridge"; return 1; }
}

# The bootstrap path (no flags) is the half that must HEAL, not only
# report -- Codex review of PR 254: both cases above call --context, so a
# regression that turned `_ensure_bridge` back into a query would leave
# them green.
boot() { CLAUDE_CODE_SESSION_ID="$1" timeout 30 bash "$SCRIPTS_DIR/comm-session-start.sh" 2>"$WORK/err" | head -n1; }

case_bootstrap_restarts_a_dead_bridge() {
    # A ping watcher, not a Monitor: this case is about the bridge on the
    # SURVIVED path, and only a comm-wake survivor takes that path now.
    local p; p="$(wake_sleeper)"; printf '%s\nsess-A\n' "$p" > "$MARKER"
    sot_bridge_stop "$NAME" 2>/dev/null || true
    local out; out="$(boot sess-A)"
    [[ "$out" == "SURVIVED handle=$NAME listener=restarted" ]] || { echo "    got '$out' (err: $(head -c 300 "$WORK/err"))"; return 1; }
    sot_bridge_running_for "$NAME" || { echo "    reported restarted with no bridge running"; return 1; }
    sot_bridge_stop "$NAME"
}
case_two_racing_starts_leave_one_bridge() {
    sot_bridge_stop "$NAME" 2>/dev/null || true
    sot_bridge_start "$NAME" "$WORK/fake-relay.sh" & local a=$!
    sot_bridge_start "$NAME" "$WORK/fake-relay.sh" & local b=$!
    wait "$a" "$b" 2>/dev/null
    sleep 0.3
    local n; n="$(pgrep -u "$(id -un)" -f "fake-relay.sh $NAME\$" 2>/dev/null | wc -l)"
    [ "$n" -le 1 ] || { echo "    $n bridge loops survived a concurrent start"; return 1; }
    sot_bridge_stop "$NAME"
}

# A daemon that does not answer must not cost a capsule row its wake path. The
# bootstrap used to resolve the endpoint and require a live `pty.screen` before
# it would spawn a watcher at all, so one silent second printed MONITOR --
# and nothing re-arms afterwards, so that session stayed on the Monitor for its
# whole life. The watcher retries the daemon itself now, so arming it is the
# honest claim; the ONLY test left in that branch is that an owner exists.
#
# The endpoint here points at a socket that does not exist, so no daemon
# anywhere can answer and the outcome cannot depend on one being up. The script
# is run by a copy of bash NAMED `claude`, which forks it as a child: that is
# the claude ancestor `sot_owner_pid` walks to, without which this branch
# legitimately refuses.
case_a_silent_daemon_still_arms_the_watcher() {
    local out="$WORK/wake.out" self="$WORK/testhost__ws-test.txt"
    local claude="$WORK/fakebin/claude" bash_bin
    bash_bin="$(command -v bash)" || return 1
    cp "$bash_bin" "$claude" || return 1
    # A leftover marker would answer SURVIVED before the branch under test ran.
    rm -f "${MARKER:?}"
    sot_write_self_file "$self" "$NAME" "$REPO" "$PROJECT_ROOT" || return 1
    SOT_COMM_SELF_FILE="$self" SOT_SOCKET="$WORK/no-such-daemon.sock" \
        CLAUDE_CODE_SESSION_ID=sess-WAKE \
        "$claude" -c 'bash "$1" >"$2" 2>&1' _ "$SCRIPTS_DIR/comm-session-start.sh" "$out"
    # Whatever it decided, do not leave a watcher behind: it is a REAL one,
    # and a leaked one makes the NEXT case's did-it-come-up check pass on the
    # wrong process. The marker names it; the sweep catches it even when the
    # marker does not, and is scoped to this suite's handle so it can never
    # match a watcher belonging to a real session.
    local w; w="$(sed -n '1p' "$MARKER" 2>/dev/null)"
    [[ "$w" =~ ^[0-9]+$ ]] && { kill "$w" 2>/dev/null; SLEEPERS+=("$w"); }
    for w in $(pgrep -f "comm-[w]ake.sh $NAME" 2>/dev/null); do kill "$w" 2>/dev/null; done
    grep -q 'WAKE: comm-wake.sh' "$out" || {
        echo "    got: $(grep -o 'BOOTSTRAP-ARM.*' "$out" | head -n1)"
        grep -q 'wake: ' "$out" && echo "    reason: $(grep -o 'wake: .*' "$out" | head -n1)"
        return 1
    }
    ! grep -q 'MONITOR:' "$out" || { echo "    a Monitor was printed as well as a WAKE line"; return 1; }
    return 0
}

# A SURVIVING MONITOR IS NOT A SURVIVING PING WATCHER. The marker is shared,
# so a live comm-watch.sh armed by THIS session satisfied the old broad read:
# the bootstrap reported SURVIVED and armed nothing, leaving the row's only
# wake path a Monitor nobody re-arms -- deaf within the half hour the harness
# gives it. The session id here is our own precisely so that identity is NOT
# what decides this; the kind of watcher is.
#
# The liveness assertion is the ruling, not decoration: falling through must
# never become reaping. A Monitor is a live wake path that may keep running
# beside the ping watcher, and the ping start claims the marker anyway.
case_a_live_monitor_is_not_survival_and_is_not_reaped() {
    local p; p="$(sleeper)"; printf '%s\nsess-M\n' "$p" > "$MARKER"
    local out; out="$(ctx sess-M)"
    [[ "$out" == "NOT SURVIVED handle=$NAME"* ]] || { echo "    got '$out'"; return 1; }
    sleep 0.3
    kill -0 "$p" 2>/dev/null || { echo "    the Monitor was reaped instead of left alone"; return 1; }
    [ "$(sed -n '1p' "$MARKER" 2>/dev/null)" = "$p" ] \
        || { echo "    the Monitor's marker was removed or rewritten"; return 1; }
    kill "$p" 2>/dev/null
    return 0
}

check "own live watcher survives, untouched" case_own_live_watcher_survives
check "a live Monitor is not survival, and is not reaped" case_a_live_monitor_is_not_survival_and_is_not_reaped
# SPAWNED IS NOT ARMED. The bootstrap used to claim WAKE the instant `nohup`
# returned, so a watcher that died at startup -- a `set -u` slip, a box with
# no jq, a bad path -- was announced as "no Monitor needed" over a session
# with no wake path at all. It now waits up to a second for a live
# comm-wake.sh for this handle and, failing that, says so loudly and arms the
# Monitor. The watcher is replaced here by one that exits immediately, through
# a scripts directory of symlinks -- the bootstrap resolves every sibling
# script, comm-lib.sh included, from its own path.
case_a_watcher_that_dies_at_startup_falls_back_loudly() {
    local d="$WORK/wake-dies" out="$WORK/wake-dies.out" f
    rm -rf "${d:?}"; mkdir -p "$d/bin"
    for f in "$SCRIPTS_DIR"/*; do ln -sf "$f" "$d/bin/$(basename "$f")"; done
    rm -f "${d:?}/bin/comm-wake.sh"
    printf '#!/bin/sh\nexit 1\n' > "$d/bin/comm-wake.sh"; chmod +x "$d/bin/comm-wake.sh"
    local self="$WORK/testhost__ws-dies.txt" claude="$WORK/fakebin/claude"
    cp "$(command -v bash)" "$claude" 2>/dev/null || return 1
    rm -f "${MARKER:?}"
    sot_write_self_file "$self" "$NAME" "$REPO" "$PROJECT_ROOT" || return 1
    SOT_COMM_SELF_FILE="$self" SOT_SOCKET="$WORK/no-such-daemon.sock" \
        CLAUDE_CODE_SESSION_ID=sess-DIES \
        "$claude" -c 'bash "$1" >"$2" 2>&1' _ "$d/bin/comm-session-start.sh" "$out"
    grep -q '^WAKE FAILED' "$out" || {
        echo "    no WAKE FAILED line: $(grep -o 'BOOTSTRAP-ARM.*' "$out" | head -n1)"
        return 1
    }
    grep -q 'MONITOR:' "$out" || { echo "    the Monitor was not armed after the failure"; return 1; }
    ! grep -q 'WAKE: comm-wake.sh' "$out" || { echo "    it claimed WAKE as well as failing"; return 1; }
    return 0
}

check "a silent daemon still arms the watcher instead of printing MONITOR" case_a_silent_daemon_still_arms_the_watcher
# DOOR THREE: a surviving MONITOR is not a surviving ping watcher. `_survived`
# is broad by design -- a Monitor writes the same marker and must read as live
# there -- so a live comm-watch.sh armed by THIS session satisfied it, the
# bootstrap set WAKE_ACTIVE and printed "no Monitor needed", and the row's only
# wake path was a Monitor nobody re-arms: deaf within the half hour the harness
# gives it. This hub's own banner reports exactly that state, so a restart
# there would have taken this path.
#
# The run must end with a ping watcher SPAWNED. The stand-in comm-wake.sh
# records that it ran and stays alive long enough for the bootstrap's own
# did-it-come-up check to find it.
case_a_surviving_monitor_still_spawns_a_ping_watcher() {
    local d="$WORK/survived-monitor" out="$WORK/survived-monitor.out" f mon
    rm -rf "${d:?}"; mkdir -p "$d/bin"
    for f in "$SCRIPTS_DIR"/*; do ln -sf "$f" "$d/bin/$(basename "$f")"; done
    rm -f "${d:?}/bin/comm-wake.sh"
    printf '#!/bin/sh\nprintf "%%s\\n" "$*" >> "%s/spawned"\nsleep 5\n' "$d" > "$d/bin/comm-wake.sh"
    chmod +x "$d/bin/comm-wake.sh"
    : > "$d/spawned"
    # A live Monitor for this handle, armed by the session we are about to be.
    mon="$(sleeper)"
    printf '%s\nsess-SURV\n' "$mon" > "$MARKER"
    local self="$WORK/testhost__ws-surv.txt" claude="$WORK/fakebin/claude"
    cp "$(command -v bash)" "$claude" 2>/dev/null || return 1
    sot_write_self_file "$self" "$NAME" "$REPO" "$PROJECT_ROOT" || return 1
    SOT_COMM_SELF_FILE="$self" SOT_SOCKET="$WORK/no-such-daemon.sock" \
        CLAUDE_CODE_SESSION_ID=sess-SURV \
        "$claude" -c 'bash "$1" >"$2" 2>&1' _ "$d/bin/comm-session-start.sh" "$out"
    local spawned_ok=0 monitor_ok=0
    [ -s "$d/spawned" ] && spawned_ok=1
    # The Monitor is a live wake path, not an orphan: read its liveness BEFORE
    # this case's own cleanup kills it.
    kill -0 "$mon" 2>/dev/null && monitor_ok=1
    for f in $(pgrep -f "$d/bin/comm-[w]ake.sh" 2>/dev/null); do kill "$f" 2>/dev/null; done
    kill "$mon" 2>/dev/null || true
    [ "$spawned_ok" = 1 ] || {
        echo "    no ping watcher was spawned beside the surviving Monitor"
        echo "    got: $(grep -o 'BOOTSTRAP-ARM.*' "$out" | head -n1)"
        return 1
    }
    grep -q 'WAKE: comm-wake.sh' "$out" || { echo "    the bootstrap did not report WAKE"; return 1; }
    ! grep -q 'WAKE FAILED' "$out" || { echo "    the spawned watcher was not seen by the arm check"; return 1; }
    [ "$monitor_ok" = 1 ] || { echo "    the surviving Monitor was reaped"; return 1; }
    return 0
}

check "a watcher that dies at startup falls back loudly" case_a_watcher_that_dies_at_startup_falls_back_loudly
check "a surviving Monitor still spawns a ping watcher" case_a_surviving_monitor_still_spawns_a_ping_watcher
check "orphan armed by another session: NOT SURVIVED and reaped" case_orphan_from_another_session_is_not_survived_and_reaped
check "legacy marker (pid only) keeps the liveness-only answer" case_legacy_marker_without_session_line_keeps_liveness_answer
check "dead pid is not survived" case_dead_pid_is_not_survived
check "no session id in the environment: liveness alone decides" case_no_session_id_in_env_trusts_liveness
check "a live bridge reads listener=up and keeps the do-not-re-listen line" case_live_bridge_is_reported_up_and_keeps_the_do_not_relisten_line
check "a dead bridge reads listener=down and is never told not to re-listen" case_dead_bridge_is_reported_down_and_never_says_do_not_relisten
# THE BRIDGE TETHER, both directions, because the owner check is the one that
# can take a box's comms down. The loop runs in a bare `bash -c` with no
# library sourced, so an owner check that calls a function which does not
# exist there takes the failure branch every time: the loop would kill its
# relay child, drop the pidfile and exit within about two seconds of starting,
# and the named receiver the daemon counts for that box would be gone. That is
# a worse outcome than the deafness this lane is fixing, so it is measured,
# not argued -- a live owner must keep its bridge, and a dead one must not.
case_a_bridge_keeps_a_live_owner_and_follows_a_dead_one() {
    local owner tries=0
    owner="$(sleeper)"
    sot_bridge_stop "$NAME" 2>/dev/null || true
    sot_bridge_start "$NAME" "$WORK/fake-relay.sh" "$owner"
    sleep 3
    # Check the FIXTURE before blaming the code: if the owner itself died (a
    # sweep from another case, a slow box), the bridge following it is correct
    # behaviour and the case has proved nothing either way.
    kill -0 "$owner" 2>/dev/null || { echo "    the fixture's owner process died; inconclusive, not a bridge failure"; return 1; }
    sot_bridge_running_for "$NAME" || { echo "    the bridge died while its owner was alive"; return 1; }
    kill "$owner" 2>/dev/null
    while [ "$tries" -lt 50 ] && sot_bridge_running_for "$NAME"; do sleep 0.2; tries=$((tries + 1)); done
    if sot_bridge_running_for "$NAME"; then
        echo "    the bridge outlived its owner"
        sot_bridge_stop "$NAME"
        return 1
    fi
    return 0
}

check "the bootstrap path restarts a dead bridge" case_bootstrap_restarts_a_dead_bridge
check "a bridge keeps a live owner and follows a dead one" case_a_bridge_keeps_a_live_owner_and_follows_a_dead_one
check "two racing starts leave at most one bridge" case_two_racing_starts_leave_one_bridge
echo; echo "$PASS passed, $FAIL failed"; [ "$FAIL" -eq 0 ]
