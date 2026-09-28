#!/usr/bin/env bash
# test-comm-wake-ping.sh — `comm-wake.sh <handle> --deliver ping`'s notice
# path: one fixed line typed for a whole batch of new directed messages
# (never the message text itself — the session reads that with
# comm-poll.sh), the selftest-only variant, the prompt-free gate, ping
# coalescing (an unread ping suppresses a second one; a cursor whose CONTENT
# already covers the pending batch's newest ts skips a second ping rather
# than firing on any cursor movement at all), the no-reply give-up (pty.screen
# unanswered 5 times in a row exits and drops the marker), the row the
# daemon does not have (a refusal is GONE, never a busy prompt), workspace-id
# derivation from $SOT_COMM_SELF_FILE, and the agent-liveness exit.
#
# Cursor starts at the inbox's END (same rule `full` mode already pins,
# proven by test-codex-watch-capsule-loop.sh): every case's inbox starts
# EMPTY and new lines are appended from inside the `sleep` stub, one poll
# cycle at a time — never pre-populated before the watcher starts.
#
# Each case runs `_comm_wake_main` in its own script file under a fresh
# sandbox $SOT_COMM_HOME, invoked via `bash script.sh` (not `bash -c` string
# splicing) so env is passed by export, not quoting -- stubs
# `_comm_wake_pty_screen`/`_comm_wake_pty_input`/`_comm_wake_row`/`sleep`/
# `kill` as needed, same seams the existing codex-watch tests stub.
# `_comm_wake_row` is the handle->row resolver the watcher asks before every
# batch, instead of waking the id frozen into its environment at spawn; a case
# that says nothing about it stubs it to that same id and so pins exactly what
# it always pinned. The owning-agent pid is
# now an argv flag (`--owner <pid>`), not a discovered function, so a case
# that wants no liveness tie simply omits `--owner`, and the one liveness
# case passes it directly.
#
# Usage: comm/core/tests/test-comm-wake-ping.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
export WAKE="$SCRIPTS_DIR/comm-wake.sh"

# Hermetic identity (2026-09-28). Each case pins the row it means to act on
# through $SOT_WORKSPACE_ID, and the row a shell may act on is now read from
# its identity first -- so a session running this suite would otherwise lend
# every case its OWN self file, and the watcher would name the runner's row
# instead of the fixture's. Nothing here owns a row; drop the inherited one.
unset SOT_COMM_SELF_FILE

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-wake-ping-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then
        echo "PASS: $desc"; PASS=$((PASS + 1))
    else
        echo "FAIL: $desc"; FAIL=$((FAIL + 1))
    fi
}

case_three_new_directed_lines_type_the_ping_once() {
    local d="$WORK/three-lines"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls" attempts="$d/pty-input.log"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["banner","❯"],"cursor":{"row":1,"col":2}}}'; }
_comm_wake_pty_input() {
    printf x >> "$calls"
    printf '%s' "\$2" | base64 -d >> "$attempts"; printf '\n' >> "$attempts"
    printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'
}
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"one"}\n{"from":"peer","to":"me","msg":"two"}\n{"from":"peer","to":"me","msg":"three"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s), want exactly 1"; cat "$attempts" 2>/dev/null; return 1; }
    # Exact match (not a substring grep): $d's own path (.../three-lines/...)
    # legitimately contains "three", so a loose grep for the message words
    # would false-positive on the sandbox path embedded in the ping text.
    local expected="[sot-comm] new message for @watchee — run $d/bin/comm-poll.sh"
    [ "$(cat "$attempts" 2>/dev/null)" = "$expected" ] || { echo "  typed text was '$(cat "$attempts" 2>/dev/null)', want '$expected'"; return 1; }
    return 0
}

case_selftest_only_batch_types_the_selftest_text() {
    local d="$WORK/selftest-only"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls" attempts="$d/pty-input.log"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() {
    printf x >> "$calls"
    printf '%s' "\$2" | base64 -d >> "$attempts"; printf '\n' >> "$attempts"
    printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'
}
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"__selftest__","to":"me","msg":"ping"}\n{"from":"__selftest__","to":"me","msg":"ping2"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s), want exactly 1"; return 1; }
    grep -q "wake selftest OK" "$attempts" || { echo "  typed text was not the selftest notice: $(cat "$attempts")"; return 1; }
    return 0
}

case_prompt_not_free_waits_then_types_once_free() {
    local d="$WORK/prompt-gate"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls" screen_calls="$d/screen.calls"
    : > "$calls"; : > "$screen_calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() {
    printf x >> "$screen_calls"
    local n; n=\$(wc -c < "$screen_calls")
    if [ "\$n" -ge 2 ]; then
        printf '%s' '{"payload":{"lines":["banner","❯"],"cursor":{"row":1,"col":2}}}'
    else
        # A dialog on screen, WITH a cursor — the blocked phase must block for
        # the reason its name gives. A cursorless reply would also block, but
        # for a different reason and one production cannot produce: every
        # capsule row's screen carries a cursor, and the only runtime that
        # answers without one returns an error payload with no lines at all.
        printf '%s' '{"payload":{"lines":["Allow this action? (y/n)"],"cursor":{"row":0,"col":24}}}'
    fi
}
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 3 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s) while gated on the prompt, want exactly 1 (once free)"; return 1; }
    local sc; sc="$(wc -c < "$screen_calls" 2>/dev/null || echo 0)"
    [ "$sc" -ge 2 ] || { echo "  the prompt was never re-checked after being not-free"; return 1; }
    # The held/recovered pair is the only trace a gate that suppresses delivery
    # leaves. Pin BOTH the presence and the cardinality: one line per
    # TRANSITION, never per cycle. Without this, a timestamp helper going
    # missing would make blocked_since empty, log every 2s into a capped log
    # and never log recovery, and every assertion above would still pass.
    # The watcher sends its own diagnostics to a durable, size-bounded log, not
    # to the caller's stderr, so that is where the pair has to be.
    local wlog="$d/state/comm-wake-watchee.log" held recovered
    [ -s "$wlog" ] || { echo "  the watcher wrote no diagnostics log at all"; return 1; }
    held="$(grep -c 'ping held since' "$wlog" 2>/dev/null)" || held=0
    recovered="$(grep -c 'prompt free again' "$wlog" 2>/dev/null)" || recovered=0
    [ "$held" -eq 1 ] || { echo "  want exactly 1 held line, got $held (one per TRANSITION, not per cycle)"; return 1; }
    [ "$recovered" -eq 1 ] || { echo "  want exactly 1 recovery line, got $recovered"; return 1; }
    grep -q 'prompt free again after being held since 2' "$wlog" 2>/dev/null \
        || { echo "  the recovery line must carry the held-since stamp"; return 1; }
    if grep -q 'now sent' "$wlog" 2>/dev/null; then
        echo "  the recovery line announces a delivery the inject had not yet attempted"; return 1
    fi
    return 0
}

case_a_row_the_daemon_does_not_have_ends_the_watcher() {
    local d="$WORK/row-gone"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls" marker="$d/state/watchee.watch"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-old SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"error":"unknown workspace: ws-old","code":"unknown_workspace"}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 4 ] || { echo "the watcher held the ping instead of noticing the row was gone" >&2; exit 9; }
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local rc=$?
    [ "$rc" -eq 0 ] || { echo "  exited $rc, want 0 (there is no row left to wake)"; return 1; }
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 0 ] || { echo "  pty.input was attempted $n time(s) against a row the daemon does not have"; return 1; }
    local wlog="$d/state/comm-wake-watchee.log"
    grep -q 'capsule row ws-old is gone' "$wlog" 2>/dev/null \
        || { echo "  the log never says the row is gone: '$(cat "$wlog" 2>/dev/null)'"; return 1; }
    if grep -q 'prompt not free' "$wlog" 2>/dev/null; then
        echo "  a destroyed row was recorded as a busy one -- the ping would be held forever"; return 1
    fi
    [ ! -f "$marker" ] || { echo "  the marker was left behind, so the next session start cannot re-arm"; return 1; }
    return 0
}

# W2 — the ping goes to the row the resolver names. The id in this watcher's
# environment was frozen at spawn, and a session that continued in another row
# kept being woken in the row it used to be in.
case_the_ping_follows_the_resolver_not_the_startup_id() {
    local d="$WORK/retarget"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local targets="$d/pty-input.targets"
    : > "$targets"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-old SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf 'ws-live\n'; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["banner","❯"],"cursor":{"row":1,"col":2}}}'; }
_comm_wake_pty_input() {
    printf '%s\n' "\$1" >> "$targets"
    printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'
}
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local got; got="$(cat "$targets" 2>/dev/null)"
    [ "$got" = "ws-live" ] \
        || { echo "  the ping was typed into '$got', want ws-live (the row the resolver names)"; return 1; }
    return 0
}

# W3 — nobody declares the handle: exit so the next session start re-arms.
# Typing into the frozen id would submit a turn into whatever row now holds it.
case_no_live_row_declaring_the_handle_exits_without_typing() {
    local d="$WORK/no-row"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls" marker="$d/state/watchee.watch"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-old SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { return 1; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["banner","❯"],"cursor":{"row":1,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 4 ] || { echo "the watcher never gave up on an unresolvable handle" >&2; exit 9; }
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local rc=$?
    [ "$rc" -eq 0 ] || { echo "  exited $rc, want 0"; return 1; }
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 0 ] || { echo "  pty.input was called $n time(s) with no row to aim at"; return 1; }
    grep -q 'no live row declares @watchee' "$d/state/comm-wake-watchee.log" 2>/dev/null \
        || { echo "  the log does not say the handle has no row"; return 1; }
    [ ! -f "$marker" ] || { echo "  the marker was left behind, so the next session start cannot re-arm"; return 1; }
    return 0
}

# W4 — two rows declare one handle, which set_agent_handle really allows.
# Refusing is the point: one of the two is a stranger's session.
case_two_rows_declaring_the_handle_refuse_to_guess() {
    local d="$WORK/two-rows"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-old SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { return 3; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["banner","❯"],"cursor":{"row":1,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 4 ] || { echo "the watcher never refused an ambiguous handle" >&2; exit 9; }
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local rc=$?
    [ "$rc" -eq 0 ] || { echo "  exited $rc, want 0"; return 1; }
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 0 ] || { echo "  pty.input was called $n time(s) on an ambiguous handle -- one of those rows is a stranger"; return 1; }
    grep -q 'two or more rows declare @watchee' "$d/state/comm-wake-watchee.log" 2>/dev/null \
        || { echo "  the log does not say the handle is ambiguous"; return 1; }
    return 0
}

# A daemon that stops answering slows this watcher down; it never ends it. The
# watcher used to exit after five unanswered probes so the session could fall
# back to the harness Monitor -- but nothing re-arms a watcher, so one silent
# second cost a box its wake path for the whole session, and the fallback is
# the Monitor this mechanism exists to replace. The immortal-watcher reason for
# that exit is gone: the owner tie ends this process with its agent.
case_five_unanswered_probes_back_off_and_keep_watching() {
    local d="$WORK/no-reply"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local screen_calls="$d/screen.calls" intervals="$d/intervals"
    : > "$screen_calls"; : > "$intervals"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf x >> "$screen_calls"; printf ''; }
turns=0
sleep() {
    printf '%s\n' "\$1" >> "$intervals"
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 10 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local rc=$?
    [ "$rc" -eq 0 ] || { echo "  exited $rc, want 0"; return 1; }
    local sc; sc="$(wc -c < "$screen_calls" 2>/dev/null || echo 0)"
    [ "$sc" -eq 10 ] || { echo "  pty.screen was probed $sc time(s) in 10 cycles, want 10 (it gave up instead of backing off)"; return 1; }
    [ "$(sed -n '5p' "$intervals")" = "2" ] || { echo "  the 5th poll waited '$(sed -n '5p' "$intervals")'s, want 2 (backoff started early)"; return 1; }
    [ "$(sed -n '6p' "$intervals")" = "30" ] || { echo "  the 6th poll waited '$(sed -n '6p' "$intervals")'s, want 30 (no backoff after five silences)"; return 1; }
    return 0
}

# The same daemon, answering again: the fast poll comes back, so an outage
# costs latency only while it lasts.
case_the_poll_speeds_up_again_once_the_daemon_answers() {
    local d="$WORK/answers-again"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls" intervals="$d/intervals"
    : > "$calls"; : > "$intervals"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
turns=0
_comm_wake_pty_screen() {
    [ "\$turns" -le 6 ] && { printf ''; return 0; }
    printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'
}
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
sleep() {
    printf '%s\n' "\$1" >> "$intervals"
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 9 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s), want 1 (the held ping once the daemon answered)"; return 1; }
    [ "$(sed -n '6p' "$intervals")" = "30" ] || { echo "  the 6th poll waited '$(sed -n '6p' "$intervals")'s, want 30"; return 1; }
    [ "$(sed -n '9p' "$intervals")" = "2" ] || { echo "  the poll stayed at '$(sed -n '9p' "$intervals")'s after the daemon answered, want 2"; return 1; }
    return 0
}

case_a_second_new_message_with_an_unmoved_cursor_pings_again() {
    local d="$WORK/second-message"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/read"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    case "\$turns" in
        1) printf '{"from":"peer","to":"me","msg":"first","ts":"2026-01-01T00:00:00Z"}\n' >> "$d/inbox/watchee.jsonl" ;;
        2) printf '{"from":"peer","to":"me","msg":"second","ts":"2026-01-01T00:00:01Z"}\n' >> "$d/inbox/watchee.jsonl" ;;
    esac
    [ "\$turns" -le 3 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    # THE INVERSION (messaging ruling §4, 2026-09-26). Turn 1 pings for
    # "first"; turn 2's "second" is a genuinely NEW line and the cursor has not
    # moved, so it MUST ping again. The old `_comm_wake_ping_outstanding` gate
    # suppressed exactly this for 600s, which made one stalled session deaf to
    # everything queued behind the message it never read. A missed ping is now
    # harmless -- the recipient's own Stop hook reads its inbox at the next turn
    # boundary -- so withholding one buys nothing and costs delivery.
    [ "$n" -eq 2 ] || { echo "  pty.input called $n time(s), want exactly 2 (a second new message with an unmoved cursor must ping again)"; return 1; }
    return 0
}

case_a_cursor_that_already_covers_the_batch_skips_a_second_ping() {
    local d="$WORK/already-read"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/read"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"old news","ts":"2026-01-01T00:00:01Z"}\n' >> "$d/inbox/watchee.jsonl"
        printf '%s' "1" > "$d/read/watchee.cursor"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    # The ONE suppression that survives: the cursor is the NUMBER of inbox lines
    # the session has been shown (comm-poll.sh writes it), so a batch it already
    # reaches was read through a real poll -- advance past it with no ping. A
    # count, not a timestamp: two frames filed in the same second are distinct
    # lines, and a ts comparison silently dropped one of them.
    [ "$n" -eq 0 ] || { echo "  pty.input called $n time(s), want 0 (the cursor already covers the pending line)"; return 1; }
    return 0
}

case_no_owner_exits_two() {
    local d="$WORK/no-owner"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
# No --owner AND no discoverable claude/codex ancestor: the refusal is about
# the owner being unknowable, so discovery is stubbed to fail rather than
# depending on whatever launched this suite.
sot_owner_pid() { return 1; }
sleep() { echo "sleep must not be called without an owner" >&2; exit 9; }
_comm_wake_main watchee --deliver ping
EOF
    bash "$d/run.sh" 2>/dev/null
    local rc=$?
    [ "$rc" -eq 2 ] || { echo "  exited $rc, want 2 (no owner discoverable: a watcher must end with the agent it wakes)"; return 1; }
    [ ! -f "$d/state/watchee.watch" ] || { echo "  a refused watcher still wrote its marker"; return 1; }
    return 0
}

case_second_start_against_a_live_marker_refuses() {
    local d="$WORK/live-marker"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local marker="$d/state/watchee.watch" planted
    # A marker naming a live process that really IS a watcher for this handle:
    # a stand-in script named comm-watch.sh, started with the handle as its
    # argument, exactly what the mutex verifies before refusing.
    mkdir -p "$d/fakebin"
    printf '#!/bin/sh\nsleep 60\n' > "$d/fakebin/comm-watch.sh"; chmod +x "$d/fakebin/comm-watch.sh"
    "$d/fakebin/comm-watch.sh" watchee >/dev/null 2>&1 & planted=$!
    printf '%s\nsession-a\n' "$planted" > "$marker"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
sleep() { echo "sleep must not be called against a live marker" >&2; exit 9; }
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local rc=$?
    kill "$planted" 2>/dev/null
    [ "$rc" -eq 4 ] || { echo "  exited $rc, want 4 (a live marker means a watcher is already running)"; return 1; }
    [ "$(sed -n '1p' "$marker" 2>/dev/null)" = "$planted" ] \
        || { echo "  the refused start overwrote the live watcher's marker"; return 1; }
    [ "$(sed -n '2p' "$marker" 2>/dev/null)" = "session-a" ] \
        || { echo "  the refused start rewrote the marker's session line"; return 1; }
    return 0
}

case_no_flag_but_a_discoverable_owner_starts() {
    local d="$WORK/discovered-owner"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
# No --owner: the owner is DISCOVERED from this process's own ancestry, which is
# what lets every leg (Codex's --deliver full included) be owned without a
# caller having to remember a flag.
sot_owner_pid() { printf '%s\n' "\$\$"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello","ts":"2026-01-01T00:00:00Z"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s), want 1 (a discovered owner is an owner)"; return 1; }
    return 0
}

case_marker_pid_that_is_not_a_watcher_is_stale() {
    local d="$WORK/reused-pid"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls" marker="$d/state/watchee.watch" planted
    : > "$calls"
    # A LIVE pid that is not a watcher at all -- what pid reuse looks like on a
    # shared home, where the marker outlives the reboot. `kill -0` alone would
    # refuse every start for this handle from here on; the identity check must
    # read it as stale and let the watcher start.
    sleep 60 >/dev/null 2>&1 & planted=$!
    printf '%s\nsession-old\n' "$planted" > "$marker"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello","ts":"2026-01-01T00:00:00Z"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local rc=$? n
    kill "$planted" 2>/dev/null
    n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$rc" -ne 4 ] || { echo "  refused to start against a reused pid that is not a watcher"; return 1; }
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s), want 1 (a stale marker must not block a start)"; return 1; }
    return 0
}

case_workspace_id_derived_from_self_file_basename() {
    local d="$WORK/derived-ws"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/self"
    : > "$d/inbox/watchee.jsonl"
    : > "$d/self/testhost__ws-derived.txt"
    local calls="$d/pty-input.calls" wsid_seen="$d/wsid.seen"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
unset SOT_WORKSPACE_ID
export SOT_COMM_HOME="$d" SOT_COMM_SELF_FILE="$d/self/testhost__ws-derived.txt"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() {
    printf x >> "$calls"
    printf '%s' "\$1" > "$wsid_seen"
    printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'
}
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s), want exactly 1 (workspace id must have resolved -- rc 3 would call it 0 times)"; return 1; }
    [ "$(cat "$wsid_seen" 2>/dev/null)" = "ws-derived" ] || { echo "  workspace id passed to pty.input was '$(cat "$wsid_seen" 2>/dev/null)', want 'ws-derived'"; return 1; }
    return 0
}

case_agent_pid_gone_exits_zero_and_removes_the_marker() {
    local d="$WORK/agent-gone"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local marker="$d/state/watchee.watch"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
kill() { [ "\$1" = "-0" ] && return 1; command kill "\$@"; }
sleep() { echo "sleep must not be called once the owner is gone" >&2; exit 9; }
_comm_wake_main watchee --deliver ping --owner 99999
EOF
    bash "$d/run.sh" 2>/dev/null
    local rc=$?
    [ "$rc" -eq 0 ] || { echo "  exited $rc, want 0 (owner gone)"; return 1; }
    [ ! -f "$marker" ] || { echo "  the liveness marker was left behind after the owner was gone"; return 1; }
    return 0
}

# The regression pin: a grey prompt suggestion (ghost text drawn after the
# cursor on an empty input) is byte-identical, in text alone, to a typed
# draft -- only the cursor tells them apart. This fixture is the shape
# measured on a live idle capsule row showing a `/compact` suggestion. Fails
# against the old glyph-only helper (which sees "❯ /compact" is not exactly
# "❯" and refuses forever); passes once the gate reads the cursor instead.
case_a_grey_prompt_suggestion_does_not_block_the_ping() {
    local d="$WORK/grey-suggestion"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯ /compact"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 3 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s) against a live prompt suggestion, want exactly 1 (ghost text must not block the ping)"; return 1; }
    return 0
}

# The invariant the gate exists for, still pinned: a genuinely half-typed
# draft pushes the cursor past the glyph by more than the suggestion offset,
# and must never be typed into.
case_a_typed_draft_still_blocks_the_ping() {
    local d="$WORK/typed-draft"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯ hello"],"cursor":{"row":0,"col":8}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 3 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 0 ] || { echo "  pty.input called $n time(s) over a half-typed draft, want exactly 0"; return 1; }
    return 0
}

# The false positive the cursor rule also closes: a bare glyph left behind on
# an earlier line (here, under an open permission dialog) must not open the
# gate just because SOME line trims to "❯" -- only the cursor's OWN line
# counts. Fails against the old glyph-only helper (any line matching was
# enough); passes once the gate anchors on the cursor's line.
case_a_dialog_over_a_stale_prompt_glyph_blocks_the_ping() {
    local d="$WORK/dialog-over-stale"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯","Allow this action? (y/n)"],"cursor":{"row":1,"col":24}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"me","msg":"hello"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 3 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 0 ] || { echo "  pty.input called $n time(s) over a stale glyph behind a dialog, want exactly 0"; return 1; }
    return 0
}

# jq exits 0 on empty stdin -- sot_prompt_free must not read that silence as
# "free". A screen we never saw is never a free prompt.
case_empty_screen_reply_is_not_a_free_prompt() {
    local d="$WORK/empty-screen"; rm -rf "$d"; mkdir -p "$d"
    cat > "$d/run.sh" <<EOF
source "$SCRIPTS_DIR/comm-lib.sh"
sot_prompt_free ""
EOF
    bash "$d/run.sh" 2>/dev/null
    local rc=$?
    # rc 1 exactly, not merely non-zero: a sourcing failure or a jq parse error
    # also exits non-zero, and either would let this case pass while testing
    # nothing. The guard's own refusal is rc 1 (jq's parse codes are 4 and 5).
    [ "$rc" -eq 1 ] || { echo "  sot_prompt_free '' exited $rc, want exactly 1 -- an unseen screen is never a free prompt"; return 1; }
    return 0
}

# The gate must not GUESS which prompt convention the renderer uses. Under a
# separator render (the measured one) an empty input's cursor is at glyph+2;
# under a no-separator render it is at glyph+1, and glyph+2 is where the cursor
# sits after ONE typed character. Accepting both columns blindly submits that
# one-character draft; accepting only glyph+2 makes a no-separator prompt deaf
# forever. Both halves are pinned here, so neither shortcut can come back.
_prompt_free_says() {   # payload -> prints "free" or "held"
    local d="$WORK/prompt-conv"; rm -rf "$d"; mkdir -p "$d"
    printf 'source "%s/comm-lib.sh"\nsot_prompt_free %s\n' "$SCRIPTS_DIR" "$1" > "$d/run.sh"
    if bash "$d/run.sh" 2>/dev/null; then printf free; else printf held; fi
}
case_the_gate_reads_the_separator_instead_of_guessing() {
    local got
    got="$(_prompt_free_says "'"'{"payload":{"lines":["❯h"],"cursor":{"row":0,"col":2}}}'"'")"
    [ "$got" = held ] || { echo "  a one-character draft on a no-separator prompt read as FREE -- typing there submits it"; return 1; }
    got="$(_prompt_free_says "'"'{"payload":{"lines":["❯"],"cursor":{"row":0,"col":1}}}'"'")"
    [ "$got" = free ] || { echo "  an EMPTY no-separator prompt read as held -- that row would never be woken"; return 1; }
    got="$(_prompt_free_says "'"'{"payload":{"lines":["❯ h"],"cursor":{"row":0,"col":3}}}'"'")"
    [ "$got" = held ] || { echo "  a one-character draft on a separator prompt read as FREE"; return 1; }
    return 0
}

case_a_second_lock_on_a_held_lock_fails() {
    # THE mutex invariant, tested deterministically rather than by racing.
    #
    # This was first written as twelve concurrent starts asserting one winner,
    # and then checked for whether it could FAIL: it cannot. A check followed
    # by an unconditional write also yields one winner under a twelve-way
    # shell race, because process startup is milliseconds while the window
    # between the check and the write is microseconds. A green race proves
    # nothing about exclusion. (The two-start case above is still worth its
    # cost, but for the OUTCOME -- one survivor, one refusal -- not for this.)
    #
    # What does separate the shapes is the behaviour when the lock is already
    # held by a live watcher-start: `mkdir` fails and the holder is verified
    # alive, so the second attempt refuses instead of reclaiming. Make the
    # reclaim unconditional and this goes red, which is the only property that
    # makes it worth running. It runs as a script named comm-wake.sh with the
    # handle in argv because that is what the holder check verifies.
    local d="$WORK/lock-excl"; rm -rf "$d"; mkdir -p "$d/state" "$d/bin"
    cat > "$d/bin/comm-wake.sh" <<EOF
source "$WAKE"
HANDLE=watchee
LOCKDIR="$d/state/watchee.watch.lock.d"
_comm_wake_lock || { echo "first lock failed on a free lock" >&2; exit 1; }
if _comm_wake_lock; then echo "SECOND-LOCK-WON" >&2; exit 2; fi
sed -n '1p' "\$LOCKDIR/pid"
EOF
    local out rc
    out="$(bash "$d/bin/comm-wake.sh" watchee 2>"$d/err")"; rc=$?
    [ "$rc" -eq 0 ] || {
        echo "  exited $rc: $(cat "$d/err")"
        grep -q SECOND-LOCK-WON "$d/err" && echo "  a second start took a lock that was already held — the lock is not exclusive"
        return 1
    }
    [ "$out" = "$(sed -n '1p' "$d/state/watchee.watch.lock.d/pid" 2>/dev/null)" ] \
        || { echo "  the lock's owner line changed under a losing attempt"; return 1; }
    return 0
}

case_cleanup_leaves_a_marker_it_does_not_own() {
    # The second half of the leak: a blind `rm` on exit removed whichever
    # marker was there, so after a lost race the DEPARTING watcher deleted the
    # WINNER's marker and left a live watcher unrecorded -- which is how one
    # race became a permanent leak instead of a transient double.
    local d="$WORK/own"; rm -rf "$d"; mkdir -p "$d/state"
    local marker="$d/state/watchee.watch"
    printf '999999\nsession-b\n' > "$marker"
    cat > "$d/foreign.sh" <<EOF
source "$WAKE"
MARKER="$marker"
_comm_wake_cleanup
EOF
    bash "$d/foreign.sh"
    [ -f "$marker" ] || { echo "  cleanup deleted a marker owned by another pid"; return 1; }
    [ "$(sed -n '1p' "$marker")" = "999999" ] \
        || { echo "  cleanup rewrote a marker it does not own"; return 1; }
    # ...and it still removes one it DOES own, or a watcher would leak its own.
    cat > "$d/mine.sh" <<EOF
source "$WAKE"
MARKER="$marker"
printf '%s\nsession-c\n' "\$\$" > "\$MARKER"
_comm_wake_cleanup
EOF
    bash "$d/mine.sh"
    [ ! -f "$marker" ] || { echo "  cleanup left behind a marker this process owned"; return 1; }
    return 0
}

# A Windows box has no inbox listener: the frontend files every inbound frame
# into ONE fe-inbox.jsonl shared by every handle on the box, while a send from
# a session on the SAME box still lands in the per-handle file. Watching only
# the per-handle file left a Windows session waking on half its mail and never
# on the half that comes from another box -- the reason a session there fell
# back to the harness Monitor. Windows is FAKED per case ($OS + $LOCALAPPDATA,
# the same seam test-win-fe-inbox-readers.sh uses), so these run on every leg.
case_a_frontend_inbox_frame_pings() {
    local d="$WORK/fe-inbox-ping"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/AppDataLocal/sot"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls" attempts="$d/pty-input.log"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
export OS=Windows_NT LOCALAPPDATA="$d/AppDataLocal"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() {
    printf x >> "$calls"
    printf '%s' "\$2" | base64 -d >> "$attempts"; printf '\n' >> "$attempts"
    printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'
}
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"watchee","text":"from another box"}\n' >> "$d/AppDataLocal/sot/fe-inbox.jsonl"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s), want exactly 1"; cat "$attempts" 2>/dev/null; return 1; }
    local expected="[sot-comm] new message for @watchee — run $d/bin/comm-poll.sh"
    [ "$(cat "$attempts" 2>/dev/null)" = "$expected" ] || { echo "  typed text was '$(cat "$attempts" 2>/dev/null)', want '$expected'"; return 1; }
    return 0
}

# The frontend inbox is shared, so `.to` is the only thing separating our mail
# from a sibling handle's on the same box -- a wake on someone else's frame
# spends a model turn on a message this session cannot even read.
case_a_frontend_frame_for_another_handle_does_not_ping() {
    local d="$WORK/fe-inbox-sibling"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/AppDataLocal/sot"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
export OS=Windows_NT LOCALAPPDATA="$d/AppDataLocal"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"someone-else","text":"not yours"}\n' >> "$d/AppDataLocal/sot/fe-inbox.jsonl"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 0 ] || { echo "  pty.input called $n time(s) for a sibling handle's frame, want 0"; return 1; }
    return 0
}

# A frame whose `.from` is not a string still wakes. The admission rule is
# jq now, and a jq program that THROWS prints nothing and exits non-zero --
# which reads here exactly like "not admitted", so one odd frame would be
# dropped in silence with no wake and no trace. Deafness is the one direction
# this must never fail in, so the sender expression cannot be allowed to throw
# on any input at all.
case_a_non_string_sender_still_wakes() {
    local d="$WORK/odd-sender"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":5,"to":"me","msg":"x"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s) for a frame with a numeric .from, want 1"; return 1; }
    return 0
}

# Mail that arrived while NO watcher was running must still be announced. The
# in-memory cursor used to start at the file's end, so the scan window held
# only what was appended after arming: a frontend-box session sat deaf for two
# and a half hours with four unread directed frames already in its inbox, and
# was rescued only when something else made it take a turn (field report,
# 2026-09-28). A ping costs ONE line whatever is behind it, so there is nothing
# to be gained by ignoring a backlog.
case_a_frame_from_before_the_watcher_started_is_announced() {
    local d="$WORK/prearm-backlog"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/read"
    printf '{"from":"peer","to":"me","msg":"filed while nothing was watching"}\n' > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() { turns=\$((turns + 1)); [ "\$turns" -le 3 ] || exit 0; }
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s) for a backlog present before arming, want exactly 1"; return 1; }
    return 0
}

# THE other half, and the regression that would get the fix above reverted: a
# batch the session genuinely READ through comm-poll.sh must not be announced
# again every time a watcher restarts. The read cursor is what separates the
# two -- unread backlog pings once, read backlog is silent.
case_a_backlog_already_read_is_not_announced_on_restart() {
    local d="$WORK/read-backlog"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/read"
    printf '{"from":"peer","to":"me","msg":"you already read this"}\n' > "$d/inbox/watchee.jsonl"
    printf '1' > "$d/read/watchee.cursor"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() { turns=\$((turns + 1)); [ "\$turns" -le 3 ] || exit 0; }
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 0 ] || { echo "  pty.input called $n time(s) for mail already read; a restart must be silent"; return 1; }
    return 0
}

# `full` keeps starting at the file's END, and this is why the two modes may
# not share one initialisation: it TYPES each frame's own text into the pane,
# so a backlog in the scan window would be retyped into the row wholesale --
# a replay, not a notice.
case_full_mode_does_not_retype_a_backlog() {
    local d="$WORK/full-backlog"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/read"
    printf '{"from":"peer","to":"me","msg":"old news"}\n{"from":"peer","to":"me","msg":"older news"}\n' > "$d/inbox/watchee.jsonl"
    local calls="$d/injects"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_capsule_inject() { printf x >> "$calls"; return 0; }
turns=0
sleep() { turns=\$((turns + 1)); [ "\$turns" -le 3 ] || exit 0; }
_comm_wake_main watchee --deliver full --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 0 ] || { echo "  full mode typed $n backlog frame(s) into the pane; it must start at the end"; return 1; }
    return 0
}

# The watcher's owner tether is sot_pid_alive, asked every cycle, so its three
# answers are pinned here: a live pid, a pid that is gone, and an argument that
# is not a pid at all (empty or not a number -- a caller that lost its owner
# must read as "no owner", never as "alive"). The msys arm cannot run on this
# platform; what this proves is that the portable arm is exactly `kill -0` and
# that nothing else in the function fires before it.
case_pid_liveness_answers_live_dead_and_nonsense() {
    local dead
    ( exit 0 ) & dead=$!; wait "$dead" 2>/dev/null
    bash -c '
        source "$1" || exit 9
        sot_pid_alive "$2" || exit 1
        sot_pid_alive "$3" && exit 2
        sot_pid_alive "not-a-pid" && exit 3
        sot_pid_alive "" && exit 4
        exit 0' _ "$SCRIPTS_DIR/comm-lib.sh" "$$" "$dead"
    local rc=$?
    case "$rc" in
        0) return 0 ;;
        1) echo "  a live pid read as gone" ;;
        2) echo "  a reaped pid read as alive" ;;
        3) echo "  a non-numeric argument read as alive" ;;
        4) echo "  an empty argument read as alive" ;;
        *) echo "  the probe itself failed (rc $rc)" ;;
    esac
    return 1
}

# TWO STARTS AT ONCE, one survivor. Two watchers for one handle ran side by
# side for seventeen hours on the hub, doubling every ping, with the guard's
# own refusal never once logged -- the check-and-claim was not atomic and its
# staleness judgement consulted only the pid the marker named, so a live
# watcher the marker did not name was invisible to every start.
#
# The two fixtures are launched as a script NAMED comm-wake.sh with the handle
# in argv, because that is what the process table must show for a scan to
# recognise a watcher at all -- the same shape a real watcher has (`bash
# /path/comm-wake.sh <handle> --deliver ping --owner N`).
case_two_starts_leave_exactly_one_watcher() {
    local d="$WORK/double-start"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/bin"
    : > "$d/inbox/watchee.jsonl"
    cat > "$d/bin/comm-wake.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    # The survivor has to STAY alive while the other start makes its attempt,
    # so this stands in for the poll pause rather than removing it.
    printf '%s\n' "\$\$" >> "$d/started"
    turns=\$((turns + 1))
    command sleep 0.4
    [ "\$turns" -le 5 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    : > "$d/started"
    bash "$d/bin/comm-wake.sh" watchee >/dev/null 2>&1 &
    bash "$d/bin/comm-wake.sh" watchee >/dev/null 2>&1 &
    wait
    local survivors refusals
    survivors="$(sort -u "$d/started" 2>/dev/null | grep -c . || true)"
    refusals="$(grep -c 'refusing to start a second' "$d/state/comm-wake-watchee.log" 2>/dev/null || true)"
    [ "$survivors" -eq 1 ] || { echo "  $survivors watcher(s) reached the poll loop, want exactly 1"; return 1; }
    [ "$refusals" -eq 1 ] || { echo "  $refusals refusal(s) logged, want exactly 1"; sed 's/^/    /' "$d/state/comm-wake-watchee.log" 2>/dev/null | head -n 4; return 1; }
    return 0
}

# WHAT ONE CYCLE COSTS over a real backlog, printed as a number so the next
# person to touch this path can see it. 1500 lines is the live inbox on the
# hub (1430) with headroom, and it is reachable in one cycle by design: the
# ping scan starts at the READ CURSOR, and a cursor past EOF clamps to 0
# whenever an inbox is trimmed, cleared or restored by hand. One jq per LINE
# made that case cost 1500 spawns inside a two-second cycle. The ceiling here
# is deliberately loose -- it is there to catch a return to per-line spawning
# (tens of seconds), not to police milliseconds on a busy box.
case_a_fifteen_hundred_line_backlog_scans_in_one_pass() {
    local d="$WORK/big-backlog"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/read"
    local i
    : > "$d/inbox/watchee.jsonl"
    for i in $(seq 1 1500); do
        printf '{"from":"peer","to":"me","msg":"line %s"}\n' "$i" >> "$d/inbox/watchee.jsonl"
    done
    local calls="$d/pty-input.calls" ticks="$d/ticks"
    : > "$calls"; : > "$ticks"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    # The gap between two ticks IS one cycle's work: the scan, the gate and
    # the inject, with no poll pause in between.
    date +%s%N >> "$ticks"
    turns=\$((turns + 1))
    [ "\$turns" -le 2 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s) for a 1500-line backlog, want exactly 1"; return 1; }
    local t1 t2 ms
    t1="$(sed -n '1p' "$ticks")"; t2="$(sed -n '2p' "$ticks")"
    [ -n "$t1" ] && [ -n "$t2" ] || { echo "  the cycle was never timed"; return 1; }
    ms=$(( (t2 - t1) / 1000000 ))
    echo "  1500-line batch: one cycle took ${ms} ms"
    [ "$ms" -lt 5000 ] || { echo "  that is a per-line cost, not a per-batch one"; return 1; }
    return 0
}

# THE FILE THAT ACTUALLY FAILED. The field report was a frontend box, where
# the mail is the frontend's shared fe-inbox.jsonl read through its own
# read/<handle>.fe.cursor -- so the backlog case has to be run against THAT
# file, not only the per-handle one, or the fix is pinned on the file that was
# never deaf.
case_a_frontend_backlog_from_before_the_watcher_is_announced() {
    local d="$WORK/fe-backlog"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/read" "$d/AppDataLocal/sot"
    : > "$d/inbox/watchee.jsonl"
    printf '{"from":"peer","to":"watchee","text":"filed while nothing was watching"}\n' \
        > "$d/AppDataLocal/sot/fe-inbox.jsonl"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
export OS=Windows_NT LOCALAPPDATA="$d/AppDataLocal"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() { turns=\$((turns + 1)); [ "\$turns" -le 3 ] || exit 0; }
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s) for a frontend backlog present before arming, want exactly 1"; return 1; }
    return 0
}

# Its twin, which is what proves the two cursors are not crossed: the same
# backlog, already read through the FRONTEND cursor, announces nothing. Point
# this at read/watchee.cursor instead and it goes red.
case_a_frontend_backlog_already_read_is_silent() {
    local d="$WORK/fe-backlog-read"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/read" "$d/AppDataLocal/sot"
    : > "$d/inbox/watchee.jsonl"
    printf '{"from":"peer","to":"watchee","text":"you already read this"}\n' \
        > "$d/AppDataLocal/sot/fe-inbox.jsonl"
    printf '1' > "$d/read/watchee.fe.cursor"
    local calls="$d/pty-input.calls"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
export OS=Windows_NT LOCALAPPDATA="$d/AppDataLocal"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf x >> "$calls"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() { turns=\$((turns + 1)); [ "\$turns" -le 3 ] || exit 0; }
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 0 ] || { echo "  pty.input called $n time(s) for frontend mail already read; a restart must be silent"; return 1; }
    return 0
}

check "three new directed lines type the ping notice exactly once" case_three_new_directed_lines_type_the_ping_once
check "a 1500-line backlog is one scan, not one per line" case_a_fifteen_hundred_line_backlog_scans_in_one_pass
check "a frontend backlog filed before the watcher is announced" case_a_frontend_backlog_from_before_the_watcher_is_announced
check "a frontend backlog already read stays silent" case_a_frontend_backlog_already_read_is_silent
# THE DEFECT ITSELF, which the two-start case above cannot reach: a watcher
# that is ALIVE while the marker names someone else. On the hub the marker
# named the later of two watchers and the earlier one -- forty-five minutes
# older -- was named by nothing, so every start judged the marker's pid and
# never saw it. Here that state is built directly: a live watcher, and a
# marker naming a pid that is gone. The old shape judged the marker stale,
# removed it, claimed it and started a second watcher beside the live one.
case_a_live_watcher_no_marker_names_still_refuses() {
    local d="$WORK/unrecorded"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/bin"
    : > "$d/inbox/watchee.jsonl"
    cat > "$d/bin/comm-wake.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() { printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
turns=0
sleep() {
    printf '%s\n' "\$\$" >> "$d/started"
    turns=\$((turns + 1))
    command sleep 0.4
    [ "\$turns" -le 20 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    : > "$d/started"
    bash "$d/bin/comm-wake.sh" watchee >/dev/null 2>&1 &
    local first=$! waited=0
    while [ ! -s "$d/started" ] && [ "$waited" -lt 40 ]; do command sleep 0.1; waited=$((waited + 1)); done
    [ -s "$d/started" ] || { kill "$first" 2>/dev/null; echo "  the first watcher never reached its poll loop"; return 1; }
    # The marker now names a pid that is gone -- the state the hub was in.
    printf '%s\n%s\n' 999999 sess-OLD > "$d/state/watchee.watch"
    bash "$d/bin/comm-wake.sh" watchee >/dev/null 2>&1
    local rc=$? survivors
    pkill -P "$first" 2>/dev/null || true
    kill "$first" 2>/dev/null || true
    survivors="$(sort -u "$d/started" 2>/dev/null | grep -c . || true)"
    [ "$rc" -eq 4 ] || { echo "  the second start exited $rc, want 4 (a refusal)"; }
    [ "$survivors" -eq 1 ] || { echo "  $survivors watcher(s) reached the poll loop, want exactly 1"; return 1; }
    grep -q 'which no marker names' "$d/state/comm-wake-watchee.log" 2>/dev/null \
        || { echo "  the refusal did not name the process-table finding"; return 1; }
    return 0
}

check "two starts at once leave exactly one watcher and one refusal" case_two_starts_leave_exactly_one_watcher
check "a live watcher no marker names still refuses a second" case_a_live_watcher_no_marker_names_still_refuses
# THE WINDOWS TIER, on a Linux leg. Everything about it except the PowerShell
# program itself is shell, and all of that is testable here with a sandboxed
# PATH: a fake `ps -W` carrying the REAL column layout (PID PPID PGID WINPID
# ...), a fake `powershell.exe` that answers only when it is handed the right
# start winpid, and a `timeout` that is present or absent per case. What these
# pin is the part that was wrong twice already — which column is read going
# out and coming back, and that every failure refuses rather than guesses.
# What they CANNOT pin is the CreationDate rule that stops the walk climbing
# into a recycled parent: that lives inside the PowerShell these fakes stand
# in for, and only a Windows box can exercise it.
_win_tier_sandbox() {
    local d="$1" with_timeout="$2" b="$1/bin" t
    rm -rf "$d"; mkdir -p "$b"
    # A PATH of our own, so "no timeout on this box" is a case rather than a
    # hypothetical -- but with the tools the tier's own shell needs.
    # `bash` and `env` are on this list because the fakes below are scripts:
    # a sandboxed PATH without them cannot start its own fixtures.
    for t in tr awk sed cat env bash sh; do
        command -v "$t" >/dev/null 2>&1 && ln -sf "$(command -v "$t")" "$b/$t"
    done
    [ "$with_timeout" = timeout ] && ln -sf "$(command -v timeout)" "$b/timeout"
    cat > "$b/ps" <<'PSEOF'
#!/usr/bin/env bash
# Only -W is answered, in msys's own column order.
[ "${1:-}" = "-W" ] || exit 1
printf '%8s %7s %7s %9s %-9s %6s %8s %s\n' PID PPID PGID WINPID TTY UID STIME COMMAND
printf '%8s %7s %7s %9s %-9s %6s %8s %s\n' "$SOT_TEST_SELF_MSYS" 1 1 "$SOT_TEST_SELF_WIN" pty0 197609 10:00:00 /usr/bin/bash
printf '%8s %7s %7s %9s %-9s %6s %8s %s\n' "$SOT_TEST_OWNER_MSYS" 1 1 "$SOT_TEST_OWNER_WIN" '?' 197609 10:00:00 'C:\Program Files\claude\claude.exe'
PSEOF
    chmod +x "$b/ps"
    cat > "$b/powershell.exe" <<'PWEOF'
#!/usr/bin/env bash
# The walk prints the owner's WINDOWS pid alone, and only for the start pid it
# was actually handed -- so a caller that passes an msys pid gets nothing.
[ "${SOT_WALK_FROM:-}" = "${SOT_TEST_SELF_WIN:-}" ] || exit 0
printf '%s\r\n' "${SOT_TEST_PS_OUT:-$SOT_TEST_OWNER_WIN}"
PWEOF
    chmod +x "$b/powershell.exe"
}

_win_tier_run() {
    local d="$1" b="$1/bin" bash_bin
    bash_bin="$(command -v bash)"
    # A runner FILE, not a nested `bash -c` string: the stand-in below is the
    # one thing this fixture cannot supply from outside, since the real
    # _sot_winpid_of reads the tier shell's own $PPID, and that pid is not
    # knowable before the shell exists.
    cat > "$d/run.sh" <<EOF
source "$SCRIPTS_DIR/comm-lib.sh" || exit 9
_sot_winpid_of() { ps -W | awk -v p="\$SOT_TEST_SELF_MSYS" '\$1 == p { print \$4; exit }'; }
_sot_owner_pid_windows
EOF
    PATH="$b" \
    SOT_TEST_SELF_MSYS="${SOT_TEST_SELF_MSYS:-4242}" SOT_TEST_SELF_WIN="${SOT_TEST_SELF_WIN:-32704}" \
    SOT_TEST_OWNER_MSYS="${SOT_TEST_OWNER_MSYS:-73528}" SOT_TEST_OWNER_WIN="${SOT_TEST_OWNER_WIN:-7992}" \
    SOT_TEST_PS_OUT="${SOT_TEST_PS_OUT-}" \
    "$bash_bin" "$d/run.sh"
}

case_the_windows_tier_maps_both_namespaces() {
    local d="$WORK/win-tier-ok" out rc
    _win_tier_sandbox "$d" timeout
    out="$(_win_tier_run "$d" 2>/dev/null)"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  the tier refused a chain it should have walked (rc $rc)"; return 1; }
    # 73528 is the MSYS pid for WINPID 7992 -- the mapping the probe box
    # measured, and the one a caller can signal.
    [ "$out" = "73528" ] || { echo "  resolved '$out', want the msys pid 73528 for winpid 7992"; return 1; }
    return 0
}

case_the_windows_tier_refuses_without_a_timeout() {
    local d="$WORK/win-tier-notimeout" rc
    _win_tier_sandbox "$d" no-timeout
    _win_tier_run "$d" >/dev/null 2>&1; rc=$?
    [ "$rc" -ne 0 ] || { echo "  an unbounded PowerShell call was made anyway"; return 1; }
    return 0
}

case_the_windows_tier_refuses_without_powershell() {
    local d="$WORK/win-tier-nops" rc
    _win_tier_sandbox "$d" timeout
    rm -f "$d/bin/powershell.exe"
    _win_tier_run "$d" >/dev/null 2>&1; rc=$?
    [ "$rc" -ne 0 ] || { echo "  refused to refuse with no PowerShell on PATH"; return 1; }
    return 0
}

case_the_windows_tier_refuses_garbled_output() {
    local d="$WORK/win-tier-garbled" rc out
    _win_tier_sandbox "$d" timeout
    out="$(SOT_TEST_PS_OUT="Get-CimInstance : The RPC server is unavailable." _win_tier_run "$d" 2>/dev/null)"; rc=$?
    [ "$rc" -ne 0 ] || { echo "  an error message was accepted as a pid: '$out'"; return 1; }
    return 0
}

check "sot_pid_alive answers live, gone and not-a-pid" case_pid_liveness_answers_live_dead_and_nonsense
check "the Windows tier maps msys and Windows pids both ways" case_the_windows_tier_maps_both_namespaces
check "the Windows tier refuses when there is no timeout to bound it" case_the_windows_tier_refuses_without_a_timeout
check "the Windows tier refuses with no PowerShell on PATH" case_the_windows_tier_refuses_without_powershell
check "the Windows tier refuses output that is not a pid" case_the_windows_tier_refuses_garbled_output
check "a frame filed before the watcher started is announced" case_a_frame_from_before_the_watcher_started_is_announced
check "a backlog already read is silent when a watcher restarts" case_a_backlog_already_read_is_not_announced_on_restart
check "full mode does not retype a backlog" case_full_mode_does_not_retype_a_backlog
check "a frame whose sender is not a string still wakes" case_a_non_string_sender_still_wakes
# Mail in BOTH inboxes inside ONE cycle is ONE wake. The ping says only that
# mail exists, so a cross-box frame and a same-box frame arriving together cost
# one typed line and one model turn -- the same promise this file's header
# makes for a burst within one file. A body that ran per source typed the
# notice twice for one batch.
case_both_inboxes_in_one_cycle_ping_once() {
    local d="$WORK/both-inboxes"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/AppDataLocal/sot"
    : > "$d/inbox/watchee.jsonl"
    local calls="$d/pty-input.calls" attempts="$d/pty-input.log"
    : > "$calls"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
export OS=Windows_NT LOCALAPPDATA="$d/AppDataLocal"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["❯"],"cursor":{"row":0,"col":2}}}'; }
_comm_wake_pty_input() {
    printf x >> "$calls"
    printf '%s' "\$2" | base64 -d >> "$attempts"; printf '\n' >> "$attempts"
    printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'
}
turns=0
sleep() {
    turns=\$((turns + 1))
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"watchee","text":"from another box"}\n' >> "$d/AppDataLocal/sot/fe-inbox.jsonl"
        printf '{"from":"sibling","to":"watchee","msg":"from this box"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 3 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local n; n="$(wc -c < "$calls" 2>/dev/null || echo 0)"
    [ "$n" -eq 1 ] || { echo "  pty.input called $n time(s) for one cycle's mail, want exactly 1"; cat "$attempts" 2>/dev/null; return 1; }
    return 0
}

# The silence budget counts CYCLES, not sources: five unanswered pty.screen
# probes, whether the mail sits in one inbox or both. A per-source body spent
# two probes a cycle and hit the limit on cycle three, slowing a frontend-box
# session's wake sooner than any other box's for the same hiccup.
case_the_silence_budget_is_five_cycles_with_both_sources_hot() {
    local d="$WORK/no-reply-both"; rm -rf "$d"; mkdir -p "$d/inbox" "$d/state" "$d/AppDataLocal/sot"
    : > "$d/inbox/watchee.jsonl"
    local screen_calls="$d/screen.calls" intervals="$d/intervals"
    : > "$screen_calls"; : > "$intervals"
    cat > "$d/run.sh" <<EOF
source "$WAKE"
export SOT_WORKSPACE_ID=ws-test SOT_COMM_HOME="$d"
export OS=Windows_NT LOCALAPPDATA="$d/AppDataLocal"
sot_daemon_endpoint() { printf fixture; }
_comm_wake_row() { printf '%s\n' "\$SOT_WORKSPACE_ID"; }
_comm_wake_pty_screen() { printf x >> "$screen_calls"; printf ''; }
turns=0
sleep() {
    turns=\$((turns + 1))
    printf '%s\n' "\$1" >> "$intervals"
    if [ "\$turns" -eq 1 ]; then
        printf '{"from":"peer","to":"watchee","text":"from another box"}\n' >> "$d/AppDataLocal/sot/fe-inbox.jsonl"
        printf '{"from":"sibling","to":"watchee","msg":"from this box"}\n' >> "$d/inbox/watchee.jsonl"
    fi
    [ "\$turns" -le 8 ] || exit 0
}
_comm_wake_main watchee --deliver ping --owner \$\$
EOF
    bash "$d/run.sh" 2>/dev/null
    local rc=$?
    [ "$rc" -eq 0 ] || { echo "  exited $rc, want 0"; return 1; }
    local sc; sc="$(wc -c < "$screen_calls" 2>/dev/null || echo 0)"
    [ "$sc" -eq 8 ] || { echo "  pty.screen was probed $sc time(s) in 8 cycles with both inboxes hot, want 8 (one per cycle)"; return 1; }
    # THE assertion that separates the two shapes: a body that probes once per
    # SOURCE spends the five silences in three cycles, so it slows down on the
    # 4th poll instead of the 6th.
    [ "$(sed -n '5p' "$intervals")" = "2" ] || { echo "  the 5th poll waited '$(sed -n '5p' "$intervals")'s, want 2"; return 1; }
    [ "$(sed -n '6p' "$intervals")" = "30" ] || { echo "  the 6th poll waited '$(sed -n '6p' "$intervals")'s, want 30"; return 1; }
    return 0
}

check "a frame the frontend files on Windows pings this session" case_a_frontend_inbox_frame_pings
check "mail in both inboxes in one cycle types the notice once" case_both_inboxes_in_one_cycle_ping_once
check "the five-probe silence budget is per cycle, not per inbox" case_the_silence_budget_is_five_cycles_with_both_sources_hot
check "a frontend frame for another handle on the box does not ping" case_a_frontend_frame_for_another_handle_does_not_ping
check "a batch that is only __selftest__ frames types the selftest notice" case_selftest_only_batch_types_the_selftest_text
check "a not-free prompt withholds the ping and types it once the prompt frees up" case_prompt_not_free_waits_then_types_once_free
check "five unanswered pty.screen probes back off instead of giving up" case_five_unanswered_probes_back_off_and_keep_watching
check "the poll speeds up again once the daemon answers" case_the_poll_speeds_up_again_once_the_daemon_answers
check "a row the daemon does not have ends the watcher instead of holding the ping" case_a_row_the_daemon_does_not_have_ends_the_watcher
check "the ping follows the resolver, not the id frozen at spawn" case_the_ping_follows_the_resolver_not_the_startup_id
check "no live row declaring the handle exits without typing" case_no_live_row_declaring_the_handle_exits_without_typing
check "two rows declaring the handle refuse to guess which to wake" case_two_rows_declaring_the_handle_refuse_to_guess
check "a second new message with an unmoved cursor pings again" case_a_second_new_message_with_an_unmoved_cursor_pings_again
check "a cursor that already covers the pending batch skips a second ping" case_a_cursor_that_already_covers_the_batch_skips_a_second_ping
check "no owner discoverable exits 2 and writes no marker" case_no_owner_exits_two
check "no --owner flag but a discoverable owner starts" case_no_flag_but_a_discoverable_owner_starts
check "a marker pid that is not a watcher is treated as stale" case_marker_pid_that_is_not_a_watcher_is_stale
check "a second start on a held lock refuses" case_a_second_lock_on_a_held_lock_fails
check "cleanup leaves a marker it does not own" case_cleanup_leaves_a_marker_it_does_not_own
check "a second start against a live marker refuses" case_second_start_against_a_live_marker_refuses
check "the workspace id derives from SOT_COMM_SELF_FILE's basename" case_workspace_id_derived_from_self_file_basename
check "the owning agent gone ends the watcher and removes its marker" case_agent_pid_gone_exits_zero_and_removes_the_marker
check "a grey prompt suggestion does not block the ping" case_a_grey_prompt_suggestion_does_not_block_the_ping
check "a typed draft still blocks the ping" case_a_typed_draft_still_blocks_the_ping
check "a dialog over a stale prompt glyph blocks the ping" case_a_dialog_over_a_stale_prompt_glyph_blocks_the_ping
check "an empty screen reply is not a free prompt" case_empty_screen_reply_is_not_a_free_prompt
check "the gate reads the separator cell instead of guessing the convention" case_the_gate_reads_the_separator_instead_of_guessing

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
