#!/usr/bin/env bash
# test-status-floor.sh — hermetic regression suite for the work-state machine
# in comm-status.sh and the three Claude hooks that drive it (prompt →
# working, tool → heartbeat, Stop → floor). ADR 0044: blue/gray = unread/read.
# No bats dependency. Runs against a temp $SOT_COMM_HOME with a pinned
# self-file ($SOT_COMM_SELF_FILE) — never touches the real ~/.sot-comm.
#
# Covers (Codex review of #223): the fourteen state scenarios, the two
# read-decide-write races (a writer committing while the floor waits for the
# lock must win), a failed registry mutation propagating a non-zero exit, and
# the Stop hook's deployment-order tolerance (soft `done` only to a
# comm-status.sh that has the soft floor).
#
# Usage: comm/core/tests/test-status-floor.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/../../adapters/claude/hooks" && pwd)"
SCRIPT_DIR="$SCRIPTS_DIR"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-status-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
trap 'rm -rf "$WORK"' EXIT

export SOT_COMM_HOME="$WORK/home"
export SOT_COMM_SELF_FILE="$WORK/self.txt"
export SOT_COMM_TEST_HOST="testhost"
unset SOT_COMM_NAME COMM_STATUS_SOFT COMM_STATUS_ORIGIN
mkdir -p "$SOT_COMM_HOME"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
# shellcheck source=../scripts/comm-lib.sh
source "$SCRIPTS_DIR/comm-lib.sh"
ensure_home
NAME="floor-test"
# The self-file must name THIS checkout's repo/root (comm-context discards a
# self-file whose root doesn't match the project it runs in), so let
# comm-context derive both first, then pin the handle against them.
eval "$("$SCRIPTS_DIR/comm-context.sh" 2>/dev/null | grep -E '^(REPO|PROJECT_ROOT)=')"
sot_write_self_file "$SOT_COMM_SELF_FILE" "$NAME" "$REPO" "$PROJECT_ROOT" || { echo "self-file write failed" >&2; exit 1; }
[ "$("$SCRIPTS_DIR/comm-context.sh" | sed -n 's/^NAME=//p')" = "$NAME" ] || { echo "context did not resolve the pinned self-file" >&2; exit 1; }

ST="$SCRIPTS_DIR/comm-status.sh"
W() { printf '%s' "$1" | bash "$HOOKS_DIR/comm-status-working.sh"; }
I() { printf '{}' | bash "$HOOKS_DIR/comm-status-idle.sh"; }
# IT TEXT [stop_hook_active]: Stop with a transcript whose last assistant message is TEXT.
IT() {
    local tr="$WORK/transcript.jsonl"
    { jq -nc '{type:"user",message:{content:"go"}}'
      jq -nc --arg t "$1" '{type:"assistant",message:{content:[{type:"text",text:$t}]}}'; } > "$tr"
    jq -nc --arg p "$tr" --argjson a "${2:-false}" '{transcript_path:$p, stop_hook_active:$a}' | bash "$HOOKS_DIR/comm-status-idle.sh"
}
summary() { jq -r --arg n "$NAME" '.agents[$n].summary // ""' "$REGISTRY"; }
# ITX TOOLS SECS TEXT [stop_hook_active]: Stop with a transcript whose turn ran TOOLS
# tool calls over SECS wall seconds after the human prompt, ending in TEXT.
ITX() {
    local tr="$WORK/transcript.jsonl" n="$1" secs="$2" i
    { jq -nc '{type:"user",timestamp:"2026-09-10T12:00:00.000Z",message:{content:"go"}}'
      for ((i=0; i<n; i++)); do
        jq -nc --arg i "$i" '{type:"assistant",timestamp:"2026-09-10T12:00:01.000Z",message:{content:[{type:"tool_use",id:("t"+$i),name:"Bash",input:{command:"ls"}}]}}'
        jq -nc --arg i "$i" '{type:"user",message:{content:[{type:"tool_result",tool_use_id:("t"+$i),content:"ok"}]}}'
      done
      jq -nc --arg t "$3" --argjson s "$secs" '{type:"assistant",timestamp:(("2026-09-10T12:00:00Z"|fromdateiso8601)+$s|todate),message:{content:[{type:"text",text:$t}]}}'; } > "$tr"
    jq -nc --arg p "$tr" --argjson a "${4:-false}" '{transcript_path:$p, stop_hook_active:$a}' | bash "$HOOKS_DIR/comm-status-idle.sh"
}
HB() { printf '{"tool_name":"Bash"}' | bash "$HOOKS_DIR/comm-status-heartbeat.sh"; }
GENUINE='{"prompt":"please do the thing"}'
RELAY='{"prompt":"[relay] from peer: ack"}'

seed() {  # STATE [turn_origin]
    jq -n --arg n "$NAME" --arg st "$1" --arg o "${2-}" \
        '{agents:{($n):({state:$st, summary:"prior", status_at:"2026-09-08T00:00:00Z", repo:"x"} + (if $o != "" then {turn_origin:$o} else {} end))}}' \
        > "$REGISTRY"
}
row() { jq -r --arg n "$NAME" '.agents[$n] | (.state // "") + "/" + (.turn_origin // "-") + "/" + (if .sticky then "sticky" else "-" end)' "$REGISTRY"; }
expect() {  # WANT LABEL
    local got; got="$(row)"
    if [ "$got" = "$1" ]; then return 0; fi
    echo "    $2: want '$1', got '$got'"; return 1
}

PASS=0; FAIL=0
check() {
    local desc="$1"; shift
    if "$@"; then PASS=$((PASS+1)); echo "PASS $desc"; else FAIL=$((FAIL+1)); echo "FAIL $desc"; fi
}

# ---- the state scenarios ----
case_user_turn_ends_blue() { seed idle; W "$GENUINE"; expect working/user/- start && I && expect done/user/- end; }
case_machine_turn_ends_gray() { seed idle; W "$RELAY"; expect working/machine/- start && I && expect idle/machine/- end; }
case_soft_done_holds_blue() { seed done; COMM_STATUS_SOFT=1 "$ST" done; expect done/-/- held; }
case_soft_done_holds_red() { seed blocked; COMM_STATUS_SOFT=1 "$ST" done; expect blocked/-/- held; }
# A live sticky marker: a HUMAN prompt paints green for the turn (marker kept),
# and the turn-end floor restores purple while the marker lives (2026-09-08).
case_sticky_waiting_survives_user_turn() { seed idle; "$ST" waiting "job"; W "$GENUINE"; expect working/user/sticky prompt && I && expect waiting/user/sticky end; }
case_explicit_idle_then_floor_is_gray() { seed idle; W "$GENUINE"; "$ST" idle; I; expect idle/user/- end; }
case_machine_wake_on_blue_stays_blue() { seed idle; W "$GENUINE"; I; W "$RELAY"; expect done/machine/- wake && HB && expect done/machine/- tool && I && expect done/machine/- end; }
case_machine_wake_on_red_stays_red() { seed idle; W "$GENUINE"; "$ST" blocked "q?"; W "$RELAY"; expect blocked/machine/- wake && HB && expect blocked/machine/- tool && I && expect blocked/machine/- end; }
case_explicit_working_in_machine_turn_on_red_ends_gray() { seed idle; W "$GENUINE"; "$ST" blocked "q?"; W "$RELAY"; "$ST" working "resuming"; expect working/machine/- explicit && I && expect idle/machine/- end; }
case_blocked_answered_ends_blue() { seed blocked; W "$GENUINE"; expect working/user/- answer && I && expect done/user/- end; }
case_blue_cleared_by_next_prompt() { seed idle; "$ST" done "shipped"; expect done/-/- explicit && W "$GENUINE"; expect working/user/- next; }
case_machine_wake_on_sticky_then_explicit_working_ends_gray() { seed idle; W "$GENUINE"; "$ST" waiting "job"; W "$RELAY"; expect waiting/machine/sticky wake && "$ST" working "finishing"; expect working/machine/- explicit && I && expect idle/machine/- end; }
case_user_prompt_on_sticky_then_explicit_working_ends_blue() { seed idle; W "$RELAY"; "$ST" waiting "job"; W "$GENUINE"; expect working/user/sticky prompt && "$ST" working "finishing"; expect working/user/- explicit && I; expect done/user/- end; }
case_machine_turn_with_tool_work_ends_gray() { seed idle; W "$RELAY"; HB; expect working/machine/- tool && I && expect idle/machine/- end; }
case_pre_field_working_row_floors_gray() { seed working; I; expect idle/-/- end; }
case_originless_soft_working_floors_gray() { seed idle; COMM_STATUS_SOFT=1 "$ST" working; expect working/machine/- start && I && expect idle/machine/- end; }
case_legacy_soft_idle_unchanged() { seed idle; W "$GENUINE"; COMM_STATUS_SOFT=1 "$ST" idle; expect idle/user/- end; }
case_working_with_live_marker_demotes_to_waiting() { seed idle; "$ST" waiting "job"; W "$GENUINE"; HB; I; expect waiting/user/sticky end; }

# ---- closing markers (2026-09-09): the marker line stamps the row ----
case_marker_done_stamps_blue_with_headline() {
    seed idle; W "$GENUINE"
    IT $'Some prose.\n\nSITREP: the docs failure is a silent unarmed proxy\n\nThe chain...' >/dev/null
    expect done/user/- state && [ "$(summary)" = "the docs failure is a silent unarmed proxy" ] || { echo "    summary '$(summary)'"; return 1; }
}
case_marker_question_stamps_red() {
    seed idle; W "$GENUINE"
    IT $'**SITREP-QUESTION: which box did you press W on?**\n\nContext...' >/dev/null
    expect blocked/user/- state && [ "$(summary)" = "which box did you press W on?" ] || { echo "    summary '$(summary)'"; return 1; }
}
case_marker_waiting_stamps_sticky_purple() {
    seed idle; W "$GENUINE"
    IT $'SITREP-WAITING:\n\nTwo FE peers, their launch argv; 15 min fallback.' >/dev/null
    expect waiting/user/sticky state && [ "$(summary)" = "Two FE peers, their launch argv; 15 min fallback." ] || { echo "    summary '$(summary)'"; return 1; }
}
case_marker_in_continuation_still_stamps() {
    seed idle; W "$GENUINE"; "$ST" blocked "q?"
    local out; out="$(IT $'SITREP-QUESTION: which port?' true)"
    [ -z "$out" ] || { echo "    unexpected output '$out'"; return 1; }
    expect blocked/user/- state && [ "$(summary)" = "which port?" ]
}
case_parked_user_turn_without_marker_is_nudged_once() {
    seed idle; W "$GENUINE"; "$ST" waiting "job"
    local out; out="$(IT 'I launched the job and will report.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block" and (.reason|test("SITREP-WAITING"))' >/dev/null || { echo "    no nudge: '$out'"; return 1; }
    expect waiting/user/sticky untouched || return 1
    out="$(IT 'still nothing' true)"
    [ -z "$out" ] || { echo "    re-nudged in continuation: '$out'"; return 1; }
    expect waiting/user/sticky end
}
case_parked_machine_turn_without_marker_is_not_nudged() {
    seed idle; W "$RELAY"; "$ST" blocked "q?"
    local out; out="$(IT 'ack received')"
    [ -z "$out" ] || { echo "    nudged a machine turn: '$out'"; return 1; }
    expect blocked/machine/- end
}
case_plain_user_turn_without_marker_floors_blue_unnudged() {
    seed idle; W "$GENUINE"
    local out; out="$(IT 'It is 14:00.')"
    [ -z "$out" ] || { echo "    nudged a plain answer: '$out'"; return 1; }
    expect done/user/- end
}

# ---- effort vs exchange (2026-09-10): a long green turn is asked once ----
case_effort_user_turn_without_marker_gets_one_soft_nudge() {
    seed idle; W "$GENUINE"
    local out; out="$(ITX 10 60 'Fixed it; all green.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block" and (.reason|test("SITREP: ")) and (.reason|test("back-and-forth"))' >/dev/null || { echo "    no soft nudge: '$out'"; return 1; }
    expect working/user/- untouched || return 1
    out="$(ITX 10 60 'That was a step in our exchange.' true)"
    [ -z "$out" ] || { echo "    re-nudged in continuation: '$out'"; return 1; }
    expect done/user/- end
}
case_long_quiet_user_turn_is_an_effort_by_duration() {
    seed idle; W "$GENUINE"
    local out; out="$(ITX 1 400 'Done after a long build.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block"' >/dev/null || { echo "    no nudge: '$out'"; return 1; }
}
case_short_exchange_turn_is_never_nudged() {
    seed idle; W "$GENUINE"
    local out; out="$(ITX 3 40 'Changed the label as you asked.')"
    [ -z "$out" ] || { echo "    nudged an exchange step: '$out'"; return 1; }
    expect done/user/- end
}
case_effort_machine_turn_is_not_nudged() {
    seed idle; W "$RELAY"
    local out; out="$(ITX 10 60 'peer handled')"
    [ -z "$out" ] || { echo "    nudged a machine turn: '$out'"; return 1; }
}
case_marker_variants_bold_after_and_heading_still_stamp() {
    seed idle; W "$GENUINE"
    IT $'**SITREP-WAITING**: the checks are rerunning\n\nOne job...' >/dev/null
    expect waiting/user/sticky bold-after && [ "$(summary)" = "the checks are rerunning" ] || { echo "    summary '$(summary)'"; return 1; }
    seed idle; W "$GENUINE"
    IT $'## SITREP: the loop is built\n\nThe chain...' >/dev/null
    expect done/user/- heading && [ "$(summary)" = "the loop is built" ] || { echo "    summary '$(summary)'"; return 1; }
}
case_turn_with_a_block_is_never_nudged_twice() {
    seed idle; W "$GENUINE"
    local out; out="$(ITX 12 600 $'SITREP-WAITING: the suite is running in `bg`\n\n- a bullet')"
    [ -z "$out" ] || { echo "    nudged a turn that had its block: '$out'"; return 1; }
    expect waiting/user/sticky stamped
}

# ---- races: the floor decides against the row as it is UNDER the lock ----
# Hold the registry lock, start the floor (it blocks on the lock; the barrier
# seam tells us it got there), commit a competing write, release, and assert
# the floor honoured the committed row rather than its pre-lock idea of it.
race() {  # SEED_STATE SEED_ORIGIN COMPETING_JQ WANT
    seed "$1" "$2"
    local barrier="$WORK/barrier.$$"; rm -f "$barrier"
    mkdir "$LOCKDIR" || return 1
    ( SOT_COMM_TEST_LOCK_BARRIER="$barrier" COMM_STATUS_SOFT=1 "$ST" done ) &
    local pid=$! i=0
    while [ ! -e "$barrier" ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done
    [ -e "$barrier" ] || { rmdir "$LOCKDIR"; kill "$pid" 2>/dev/null; echo "    floor never reached the lock"; return 1; }
    jq --arg n "$NAME" "$3" "$REGISTRY" > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
    rmdir "$LOCKDIR"
    wait "$pid"
    expect "$4" after-race
}
case_race_done_committed_while_floor_waits_is_kept() { race idle "" '.agents[$n].state = "done"' done/-/-; }
case_race_machine_start_while_floor_waits_ends_gray() { race working user '.agents[$n] += {state:"working", turn_origin:"machine"}' idle/machine/-; }

# ---- a failed mutation is a failed script ----
# A directory squatting on the tmp path makes the jq redirect fail; the row
# must be untouched and the exit non-zero, on both write paths.
case_failed_state_write_exits_nonzero() {
    seed idle; mkdir "$REGISTRY.tmp"
    local rc=0
    COMM_STATUS_SOFT=1 COMM_STATUS_ORIGIN=user "$ST" working 2>/dev/null || rc=$?
    rmdir "$REGISTRY.tmp"
    [ "$rc" -ne 0 ] || { echo "    exit was 0"; return 1; }
    expect idle/-/- untouched
}
case_failed_origin_write_exits_nonzero() {
    seed blocked user; mkdir "$REGISTRY.tmp"
    local rc=0
    COMM_STATUS_SOFT=1 COMM_STATUS_ORIGIN=machine "$ST" working 2>/dev/null || rc=$?
    rmdir "$REGISTRY.tmp"
    [ "$rc" -ne 0 ] || { echo "    exit was 0"; return 1; }
    expect blocked/user/- untouched
}

# ---- Stop hook deployment-order tolerance ----
stub_home() {  # HAS_SOFT_FLOOR(0|1) -> prints the argv log path
    local h="$WORK/stub$1"; rm -rf "$h"; mkdir -p "$h/bin"
    cp "$SCRIPTS_DIR/comm-context.sh" "$h/bin/"
    { echo '#!/usr/bin/env bash'; [ "$1" = 1 ] && echo '# soft_floor marker'; echo "printf '%s\n' \"\$1\" >> '$h/argv.log'"; } > "$h/bin/comm-status.sh"
    chmod +x "$h/bin/comm-status.sh"
    cp "$REGISTRY" "$h/registry.json"
    echo "$h"
}
case_stop_hook_sends_done_only_to_a_floor_aware_script() {
    seed idle
    local new old
    new="$(stub_home 1)"; SOT_COMM_HOME="$new" I
    old="$(stub_home 0)"; SOT_COMM_HOME="$old" I
    [ "$(cat "$new/argv.log")" = done ] || { echo "    new script got '$(cat "$new/argv.log")'"; return 1; }
    [ "$(cat "$old/argv.log")" = idle ] || { echo "    old script got '$(cat "$old/argv.log")'"; return 1; }
}

check "a genuine user turn ends blue" case_user_turn_ends_blue
check "a machine-started turn ends gray" case_machine_turn_ends_gray
check "the floor holds an explicit done" case_soft_done_holds_blue
check "the floor holds blocked" case_soft_done_holds_red
check "sticky waiting paints green through a user turn and returns to purple at the floor" case_sticky_waiting_survives_user_turn
check "explicit idle then the floor stays gray" case_explicit_idle_then_floor_is_gray
check "a machine wake on a blue row stays blue through tool work and Stop" case_machine_wake_on_blue_stays_blue
check "a machine wake on a red row stays red through tool work and Stop" case_machine_wake_on_red_stays_red
check "explicit working inside a machine turn on a red row ends gray (no stale origin)" case_explicit_working_in_machine_turn_on_red_ends_gray
check "blocked answered by the user ends blue" case_blocked_answered_ends_blue
check "explicit done is cleared by the next genuine prompt" case_blue_cleared_by_next_prompt
check "machine wake on sticky purple then explicit working ends gray" case_machine_wake_on_sticky_then_explicit_working_ends_gray
check "user prompt on sticky purple then explicit working ends blue" case_user_prompt_on_sticky_then_explicit_working_ends_blue
check "a machine turn with tool work ends gray" case_machine_turn_with_tool_work_ends_gray
check "a working row from before the field existed floors gray" case_pre_field_working_row_floors_gray
check "an origin-less soft working (old prompt hook) floors gray" case_originless_soft_working_floors_gray
check "legacy soft idle still floors gray" case_legacy_soft_idle_unchanged
check "a working row with a live marker demotes to waiting at the floor" case_working_with_live_marker_demotes_to_waiting
check "SITREP: stamps blue with the headline as summary" case_marker_done_stamps_blue_with_headline
check "SITREP-QUESTION: (bold-wrapped) stamps red with the question" case_marker_question_stamps_red
check "SITREP-WAITING: alone takes the next line and sets sticky purple" case_marker_waiting_stamps_sticky_purple
check "a marker in a stop-hook continuation still stamps, no nudge" case_marker_in_continuation_still_stamps
check "a human turn ending parked without a marker is nudged once, row untouched" case_parked_user_turn_without_marker_is_nudged_once
check "a machine turn ending parked without a marker is not nudged" case_parked_machine_turn_without_marker_is_not_nudged
check "a plain human answer without a marker floors blue, no nudge" case_plain_user_turn_without_marker_floors_blue_unnudged
check "an effort turn (many tool calls) ending green with no marker gets one soft nudge, then floors" case_effort_user_turn_without_marker_gets_one_soft_nudge
check "a long quiet turn is an effort by duration" case_long_quiet_user_turn_is_an_effort_by_duration
check "a short exchange step is never nudged" case_short_exchange_turn_is_never_nudged
check "an effort on a machine wake is not nudged" case_effort_machine_turn_is_not_nudged
check "marker variants (bold closing before the colon, a heading) still stamp" case_marker_variants_bold_after_and_heading_still_stamp
check "a turn that already has its block is never nudged, whatever the block says" case_turn_with_a_block_is_never_nudged_twice
check "race: a done committed while the floor waits for the lock is kept" case_race_done_committed_while_floor_waits_is_kept
check "race: a machine start committed while the floor waits ends gray, not blue" case_race_machine_start_while_floor_waits_ends_gray
check "a failed state write exits non-zero and leaves the row untouched" case_failed_state_write_exits_nonzero
check "a failed origin-only write exits non-zero and leaves the row untouched" case_failed_origin_write_exits_nonzero
check "the Stop hook sends soft done only to a floor-aware comm-status.sh" case_stop_hook_sends_done_only_to_a_floor_aware_script

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
