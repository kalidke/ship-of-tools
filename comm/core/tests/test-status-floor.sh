#!/usr/bin/env bash
# test-status-floor.sh — hermetic regression suite for the work-state machine:
# comm-status.sh (the reduction) and the Claude hooks that drive it (prompt →
# floor, tool → heartbeat, Stop → floor+marker). ADR 0044 amendment
# (2026-09-19): the registry row is a SET OF FACTS (floor/question/waiting/
# done/note), reduced to one display `state` + `summary` on every write. This
# file is the executable spec of that reduction plus the lifecycle built on
# top of it (turn start/end, closing markers, the nudge, the turn auditor).
# No bats dependency. Runs against a temp $SOT_COMM_HOME with a pinned
# self-file ($SOT_COMM_SELF_FILE) — never touches the real ~/.sot-comm.
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
unset SOT_COMM_NAME COMM_STATUS_ORIGIN CLAUDE_CODE_SESSION_ID
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

# comm-status-heartbeat.sh and comm-status-idle.sh's artifact-audit path
# resolve a sibling script next to THEMSELVES ($SELF_DIR/comm-context.sh,
# $SELF_DIR/comm-turn-auditor.sh) — that only lines up once hooks and
# scripts are deployed flat into one ~/.sot-comm/bin/ (update_comm), not in
# this checkout where hooks/ and core/scripts/ are separate directories.
# Flatten a symlink dir once, up front, so every helper below can use it.
FLAT_BIN_DIR="$WORK/flat-bin"; mkdir -p "$FLAT_BIN_DIR"
ln -s "$HOOKS_DIR/comm-status-heartbeat.sh" "$FLAT_BIN_DIR/comm-status-heartbeat.sh"
ln -s "$HOOKS_DIR/comm-status-idle.sh" "$FLAT_BIN_DIR/comm-status-idle.sh"
ln -s "$SCRIPTS_DIR/comm-turn-auditor.sh" "$FLAT_BIN_DIR/comm-turn-auditor.sh"
ln -s "$SCRIPTS_DIR/comm-context.sh" "$FLAT_BIN_DIR/comm-context.sh"
ln -s "$SCRIPTS_DIR/comm-lib.sh" "$FLAT_BIN_DIR/comm-lib.sh"
ln -s "$SCRIPTS_DIR/comm-status.sh" "$FLAT_BIN_DIR/comm-status.sh"

W() { printf '%s' "$1" | bash "$HOOKS_DIR/comm-status-working.sh"; }
I() { printf '{}' | bash "$HOOKS_DIR/comm-status-idle.sh"; }
# B: the PreToolUse AskUserQuestion hook (sends blocked, then stop).
B() { printf '{"tool_name":"AskUserQuestion"}' | bash "$HOOKS_DIR/comm-status-blocked.sh"; }
# HB: a heartbeat tool call (PostToolUse, tool_name=Bash). HBQ: the
# AskUserQuestion ANSWER's PostToolUse. Both route through FLAT_BIN_DIR so
# the hook's own NAME resolution (SELF_DIR/comm-context.sh) succeeds. HB
# clears the 10s early-throttle tick first: each HB call in a test is its
# own tool call in its own right, not a repeat within one live turn. HBQ
# does NOT clear it — the answer branch runs before that throttle check
# (review finding 2026-09-19), so a call must prove it fires even with a
# fresh tick left over from an earlier HB/B call in the same test.
HB() {
    rm -f "$SOT_COMM_HOME"/state/hb-*.tick 2>/dev/null
    printf '{"tool_name":"Bash"}' | bash "$FLAT_BIN_DIR/comm-status-heartbeat.sh"
}
HBQ() {
    printf '{"tool_name":"AskUserQuestion"}' | bash "$FLAT_BIN_DIR/comm-status-heartbeat.sh"
}
# IT TEXT [stop_hook_active]: Stop with a transcript whose last assistant message is TEXT.
# ITL TEXT PAYLOAD_MSG — the transcript holds only an earlier text record; the
# closing reply exists solely as the Stop payload's last_assistant_message
# (the hook fired before the transcript flush).
ITL() {
    local tr="$WORK/transcript.jsonl"
    { jq -nc '{type:"user",message:{content:"go"}}'
      jq -nc --arg t "$1" '{type:"assistant",message:{content:[{type:"text",text:$t}]}}'; } > "$tr"
    jq -nc --arg p "$tr" --arg m "$2" '{transcript_path:$p, stop_hook_active:false, last_assistant_message:$m}' | bash "$HOOKS_DIR/comm-status-idle.sh"
}
IT() {
    local tr="$WORK/transcript.jsonl"
    { jq -nc '{type:"user",message:{content:"go"}}'
      jq -nc --arg t "$1" '{type:"assistant",message:{content:[{type:"text",text:$t}]}}'; } > "$tr"
    jq -nc --arg p "$tr" --argjson a "${2:-false}" '{transcript_path:$p, stop_hook_active:$a}' | bash "$HOOKS_DIR/comm-status-idle.sh"
}
# ITP PROMPT TEXT [stop_hook_active]: like IT, but the transcript's OWN prompt
# record carries PROMPT instead of the literal "go" -- simulates a turn whose
# UserPromptSubmit hook never fired (a harness-injected wake that skipped the
# hook), so the registry's floor is whatever an EARLIER genuine prompt left
# it while the actual prompt record here is machine-shaped.
ITP() {
    local tr="$WORK/transcript.jsonl"
    { jq -nc --arg p "$1" '{type:"user",message:{content:$p}}'
      jq -nc --arg t "$2" '{type:"assistant",message:{content:[{type:"text",text:$t}]}}'; } > "$tr"
    jq -nc --arg p "$tr" --argjson a "${3:-false}" '{transcript_path:$p, stop_hook_active:$a}' | bash "$HOOKS_DIR/comm-status-idle.sh"
}
# IT2 TEXT1 TEXT2 [stop_hook_active]: like IT, but the closing reply is split
# across TWO separate assistant transcript records (TEXT1 then TEXT2), as a
# streaming harness can do for one logical turn.
IT2() {
    local tr="$WORK/transcript.jsonl"
    { jq -nc '{type:"user",message:{content:"go"}}'
      jq -nc --arg t "$1" '{type:"assistant",message:{content:[{type:"text",text:$t}]}}'
      jq -nc --arg t "$2" '{type:"assistant",message:{content:[{type:"text",text:$t}]}}'; } > "$tr"
    jq -nc --arg p "$tr" --argjson a "${3:-false}" '{transcript_path:$p, stop_hook_active:$a}' | bash "$HOOKS_DIR/comm-status-idle.sh"
}
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
GENUINE='{"prompt":"please do the thing"}'
RELAY='{"prompt":"[relay] from peer: ack"}'
TEAMMATE='{"prompt":"Another Claude session sent a message:\\n<teammate-message teammate_id=x>done</teammate-message>"}'
STOPBACK='{"prompt":"Stop hook feedback:\\nYour row ends this turn as waiting"}'

# seed STATE [turn_origin] — a legacy-shaped row (no facts): the pre-amendment
# field names, for the deletion/migration cases. Every OTHER case starts from
# `seed idle`, a facts-free row.
seed() {
    jq -n --arg n "$NAME" --arg st "$1" --arg o "${2-}" \
        '{agents:{($n):({state:$st, summary:"prior", status_at:"2026-09-08T00:00:00Z", repo:"x"} + (if $o != "" then {turn_origin:$o} else {} end))}}' \
        > "$REGISTRY"
}
# seed_facts JSON — a fresh row with the given facts merged in RAW (no
# reduction applied): the reduction cases below run a verb afterward
# specifically to trigger it.
seed_facts() {
    jq -n --arg n "$NAME" --argjson f "$1" \
        '{agents:{($n): ({summary:"prior", status_at:"2026-09-08T00:00:00Z", repo:"x"} + $f)}}' \
        > "$REGISTRY"
}
row() { jq -r --arg n "$NAME" '.agents[$n] | (.state // "") + "/" + (.floor // "-") + "/" + (if .question then "q" else "-" end) + "/" + (if .waiting then "w" else "-" end) + "/" + (if .done then "d" else "-" end)' "$REGISTRY"; }
summ() { jq -r --arg n "$NAME" '.agents[$n].summary // ""' "$REGISTRY"; }
status_at() { jq -r --arg n "$NAME" '.agents[$n].status_at // ""' "$REGISTRY"; }
expect() {  # WANT LABEL — WANT is state/floor/q/w/d ('-' for an absent flag)
    local got; got="$(row)"
    if [ "$got" = "$1" ]; then return 0; fi
    echo "    $2: want '$1', got '$got'"; return 1
}
# floor_now: reach a floored terminal state on a row that may currently be
# parked (question/done set) from a HUMAN turn — a single marker-less Stop
# there is a NUDGE, not a floor (a human turn owes its closing block), so
# this runs the Stop hook twice: the nudge, then a loop-guarded continuation
# (stop_hook_active=true), which floors unconditionally without re-nudging —
# exactly the two calls a real turn produces (nudge, then the model's reply).
floor_now() { IT 'ok.' >/dev/null; IT 'ok.' true >/dev/null; }

PASS=0; FAIL=0
check() {
    local desc="$1"; shift
    if "$@"; then PASS=$((PASS+1)); echo "PASS $desc"; else FAIL=$((FAIL+1)); echo "FAIL $desc"; fi
}

# ==== the reduction (comm-status.sh's status_txn), one case per table row ===
# For a floor-absent row, `stop` changes no fact but still recomputes
# state/summary (every verb does) — a neutral vehicle for asserting the
# reduction alone. For the floor row, `W RELAY` (floor=machine, clears
# nothing) is the vehicle; asserted before any stop.
case_reduction_question_no_floor_is_blocked() {
    seed_facts '{"question":"which port?"}'
    "$ST" stop >/dev/null
    expect blocked/-/q/-/- reduced && [ "$(summ)" = "which port?" ]
}
case_reduction_floor_outranks_question_waiting_done() {
    seed_facts '{"question":"q?","waiting":"job","done":true,"note":"finishing up"}'
    W "$RELAY" >/dev/null
    expect working/machine/q/w/d reduced && [ "$(summ)" = "finishing up" ]
}
case_reduction_waiting_outranks_done() {
    seed_facts '{"waiting":"the build","done":true,"note":"stale"}'
    "$ST" stop >/dev/null
    expect waiting/-/-/w/d reduced && [ "$(summ)" = "the build" ]
}
case_reduction_done_alone() {
    seed_facts '{"done":true,"note":"shipped it"}'
    "$ST" stop >/dev/null
    expect done/-/-/-/d reduced && [ "$(summ)" = "shipped it" ]
}
case_reduction_nothing_is_idle() {
    seed_facts '{"note":"last thing I said"}'
    "$ST" stop >/dev/null
    expect idle/-/-/-/- reduced && [ "$(summ)" = "last thing I said" ]
}

# ==== lifecycle, built on the reduction =====================================
case_user_turn_ends_blue() {
    seed idle; W "$GENUINE"; expect working/user/-/-/- start && I && expect done/-/-/-/d end
}
case_machine_turn_ends_gray() {
    seed idle; W "$RELAY"; expect working/machine/-/-/- start && I && expect idle/-/-/-/- end
}
case_teammate_report_is_a_machine_turn() {
    seed idle; W "$TEAMMATE"; expect working/machine/-/-/- start && I && expect idle/-/-/-/- end
}
case_stop_hook_sendback_is_a_machine_turn() {
    seed idle; W "$STOPBACK"; expect working/machine/-/-/- start && I && expect idle/-/-/-/- end
}
case_blue_survives_machine_wake() {
    seed idle; W "$GENUINE"; I; expect done/-/-/-/d after-user-turn || return 1
    W "$RELAY"; expect working/machine/-/-/d wake && I && expect done/-/-/-/d end
}
case_blue_cleared_by_next_user_prompt() {
    seed idle; W "$GENUINE"; I; expect done/-/-/-/d after-user-turn || return 1
    W "$GENUINE"; expect working/user/-/-/- no-d
}
case_question_during_running_turn_is_green() {
    seed idle; W "$GENUINE"
    "$ST" blocked "the question?" >/dev/null
    expect working/user/q/-/- mid-turn && [ "$(summ)" = "the question?" ] || return 1
    # A single marker-less Stop here is a NUDGE (a human turn owes its
    # closing block for a parked row), not a floor — floor_now runs the
    # nudge-then-continuation pair a real turn produces.
    floor_now; expect blocked/-/q/-/- end && [ "$(summ)" = "the question?" ]
}
case_machine_wake_on_red_goes_green_returns_at_stop() {
    seed idle; W "$GENUINE"; "$ST" blocked "the question?" >/dev/null; floor_now
    expect blocked/-/q/-/- parked || return 1
    W "$RELAY"; expect working/machine/q/-/- wake || return 1
    IT "ack, noted." >/dev/null; expect blocked/-/q/-/- end
}
case_human_answer_clears_question_ends_blue() {
    seed idle; W "$GENUINE"; "$ST" blocked "the question?" >/dev/null; floor_now
    expect blocked/-/q/-/- parked || return 1
    W "$GENUINE"; expect working/user/-/-/- answered || return 1
    I; expect done/-/-/-/d end
}
case_red_over_purple() {
    seed idle
    "$ST" waiting "the job" >/dev/null
    "$ST" blocked "the question?" >/dev/null
    expect blocked/-/q/w/- question-over-wait && [ "$(summ)" = "the question?" ] || return 1
    W "$GENUINE"; expect working/user/-/w/- answered || return 1
    I; expect waiting/-/-/w/- end && [ "$(summ)" = "the job" ]
}
case_purple_survives_user_and_machine_turns() {
    seed idle
    "$ST" waiting "the job" >/dev/null
    W "$GENUINE"; I; expect waiting/-/-/w/- after-user-turn || return 1
    W "$RELAY"; I; expect waiting/-/-/w/- after-machine-turn
}
case_explicit_working_clears_waiting_and_question() {
    # A declaration never sets `floor` (only the `prompt` EVENT does), so a
    # floor must already be running for the cleared row to display
    # `working` rather than trivially `idle`.
    seed idle; W "$GENUINE"
    "$ST" waiting "the job" >/dev/null; "$ST" blocked "the question?" >/dev/null
    "$ST" working >/dev/null; expect working/user/-/-/- cleared
}
case_explicit_idle_clears_waiting_and_question() {
    seed idle
    "$ST" waiting "the job" >/dev/null; "$ST" blocked "the question?" >/dev/null
    "$ST" idle >/dev/null; expect idle/-/-/-/- cleared
}
case_explicit_done_clears_waiting_and_question() {
    seed idle
    "$ST" waiting "the job" >/dev/null; "$ST" blocked "the question?" >/dev/null
    "$ST" done >/dev/null; expect done/-/-/-/d cleared
}
case_explicit_done_then_stop_stays_done() {
    seed idle; "$ST" done "shipped" >/dev/null; "$ST" stop >/dev/null
    expect done/-/-/-/d stays
}
case_explicit_idle_then_stop_stays_idle() {
    seed idle; "$ST" idle >/dev/null; "$ST" stop >/dev/null
    expect idle/-/-/-/- stays
}
# A teammate's or subagent's tool call sharing this session id can land
# moments before the owner answers, leaving the heartbeat's 10s throttle
# tick fresh right when the answer's PostToolUse fires (review finding
# 2026-09-19). HB sets that tick and B never touches it, so by the time HBQ
# runs the tick is still well under 10s old — HBQ deliberately does NOT
# clear it (unlike HB) so this proves the AskUserQuestion branch fires
# before that throttle check, not because the test cleared the obstacle.
case_ask_user_question_within_throttle_window() {
    seed idle; W "$GENUINE"
    HB
    B
    expect blocked/-/q/-/- mid-turn-red || return 1
    HBQ
    expect working/user/-/-/- answered || return 1
    I; expect done/-/-/-/d end
}
case_tool_call_on_floorless_row_changes_nothing() {
    seed idle
    local before after
    before="$(status_at)"
    HB
    after="$(status_at)"
    expect idle/-/-/-/- unchanged || return 1
    [ "$before" = "$after" ] || { echo "    status_at changed: $before -> $after"; return 1; }
}
case_tool_call_refreshes_old_stamp_only() {
    seed idle; W "$GENUINE"
    jq --arg n "$NAME" --arg t "2026-09-08T00:00:00Z" '.agents[$n].status_at = $t' "$REGISTRY" > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
    HB
    local at1; at1="$(status_at)"
    [ "$at1" != "2026-09-08T00:00:00Z" ] || { echo "    status_at not refreshed"; return 1; }
    expect working/user/-/-/- floor-kept || return 1
    HB
    local at2; at2="$(status_at)"
    [ "$at1" = "$at2" ] || { echo "    status_at changed on a fresh stamp: $at1 -> $at2"; return 1; }
}
case_headless_child_hooks_stand_down() {
    seed idle; W "$GENUINE"; "$ST" blocked "q?" >/dev/null; floor_now
    expect blocked/-/q/-/- parked || return 1
    SOT_COMM_HOOKS=off W "$GENUINE"; expect blocked/-/q/-/- child-prompt || return 1
    SOT_COMM_HOOKS=off HB; expect blocked/-/q/-/- child-tool || return 1
    SOT_COMM_HOOKS=off I; expect blocked/-/q/-/- child-stop
}
case_pre_field_working_row_floors_gray() {
    seed working user
    I
    expect idle/-/-/-/- end || return 1
    local legacy; legacy="$(jq -r --arg n "$NAME" '.agents[$n] | (has("turn_origin") or has("sticky") or has("sticky_at"))' "$REGISTRY")"
    [ "$legacy" = "false" ] || { echo "    legacy keys survived"; return 1; }
}
case_short_exchange_on_waiting_row_never_nudged() {
    seed idle; "$ST" waiting "the job" >/dev/null; W "$GENUINE"
    local out; out="$(IT 'sure.')"
    [ -z "$out" ] || { echo "    unexpected nudge: '$out'"; return 1; }
    expect waiting/-/-/w/- end
}

# ---- closing markers (2026-09-09): the marker line stamps the row ----
case_marker_done_stamps_blue_with_headline() {
    seed idle; W "$GENUINE"
    IT $'Some prose.\n\nSITREP: the docs failure is a silent unarmed proxy\n\nThe chain...' >/dev/null
    expect done/-/-/-/d state && [ "$(summ)" = "the docs failure is a silent unarmed proxy" ] || { echo "    summary '$(summ)'"; return 1; }
}
case_marker_question_stamps_red() {
    seed idle; W "$GENUINE"
    IT $'**SITREP-QUESTION: which box did you press W on?**\n\nContext...' >/dev/null
    expect blocked/-/q/-/- state && [ "$(summ)" = "which box did you press W on?" ] || { echo "    summary '$(summ)'"; return 1; }
}
case_marker_waiting_stamps_purple() {
    seed idle; W "$GENUINE"
    IT $'SITREP-WAITING:\n\nTwo FE peers, their launch argv; 15 min fallback.' >/dev/null
    expect waiting/-/-/w/- state && [ "$(summ)" = "Two FE peers, their launch argv; 15 min fallback." ] || { echo "    summary '$(summ)'"; return 1; }
}
case_marker_in_continuation_still_stamps() {
    seed idle; W "$GENUINE"; "$ST" blocked "q?" >/dev/null
    local out; out="$(IT $'SITREP-QUESTION: which port?' true)"
    [ -z "$out" ] || { echo "    unexpected output '$out'"; return 1; }
    expect blocked/-/q/-/- state && [ "$(summ)" = "which port?" ]
}
# 2026-09-14 live incident: a long reply's closing SITREP-WAITING: line was
# missed and nudged as "no closing marker" -- the reply streamed across two
# assistant transcript records and only the LAST record was ever searched.
case_marker_split_across_assistant_records_still_stamps() {
    seed idle; W "$GENUINE"
    local out
    out="$(IT2 $'Some analysis of the failure...\n\nSITREP-WAITING: the suite is rerunning in the background' \
        'One more thing: will report back once it lands.')"
    [ -z "$out" ] || { echo "    unexpected nudge: '$out'"; return 1; }
    expect waiting/-/-/w/- state && [ "$(summ)" = "the suite is rerunning in the background" ] || { echo "    summary '$(summ)'"; return 1; }
}
case_marker_only_in_stop_payload_still_stamps() {
    seed idle; W "$GENUINE"
    local out
    out="$(ITL 'Working on it.' 'SITREP-WAITING: the build is running')"
    [ -z "$out" ] || { echo "    unexpected nudge: '$out'"; return 1; }
    expect waiting/-/-/w/- state && [ "$(summ)" = "the build is running" ] || { echo "    summary '$(summ)'"; return 1; }
}
case_parked_user_turn_blocked_without_marker_is_nudged_once() {
    seed idle; W "$GENUINE"; "$ST" blocked "q?" >/dev/null
    expect working/user/q/-/- pre-stop
    local out; out="$(IT 'I will ask now.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block" and (.reason|test("SITREP-QUESTION"))' >/dev/null || { echo "    no nudge: '$out'"; return 1; }
    expect working/user/q/-/- untouched || return 1
    out="$(IT 'still nothing' true)"
    [ -z "$out" ] || { echo "    re-nudged in continuation: '$out'"; return 1; }
    expect blocked/-/q/-/- end
}
case_parked_user_turn_done_without_marker_is_nudged_once() {
    seed idle; W "$GENUINE"; "$ST" done "shipped it" >/dev/null
    expect working/user/-/-/d pre-stop
    local out; out="$(IT 'all set.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block" and (.reason|test("SITREP: "))' >/dev/null || { echo "    no nudge: '$out'"; return 1; }
    expect working/user/-/-/d untouched || return 1
    out="$(IT 'still nothing' true)"
    [ -z "$out" ] || { echo "    re-nudged in continuation: '$out'"; return 1; }
    expect done/-/-/-/d end
}
case_parked_machine_turn_without_marker_is_not_nudged() {
    seed idle; W "$RELAY"; "$ST" blocked "q?" >/dev/null
    local out; out="$(IT 'ack received')"
    [ -z "$out" ] || { echo "    nudged a machine turn: '$out'"; return 1; }
    expect blocked/-/q/-/- end
}
# 2026-09-15: the prompt hook missed this wake entirely (a harness-injected
# machine message that never fired UserPromptSubmit) -- the registry still
# says floor=user from the earlier genuine prompt, but the transcript's own
# last prompt record is machine-shaped. The Stop hook must classify it
# itself: no nudge, and the registry's floor gets corrected too (so a later
# turn floors gray, not blue).
case_parked_row_with_missed_prompt_hook_is_not_nudged() {
    seed idle; W "$GENUINE"; "$ST" blocked "q?" >/dev/null
    local out
    out="$(ITP 'Another Claude session sent a message: <teammate-message teammate_id=x>report</teammate-message>' 'ack, noted.')"
    [ -z "$out" ] || { echo "    nudged despite a machine-shaped prompt record: '$out'"; return 1; }
    expect blocked/-/q/-/- end
}
# Same shape, but the transcript's last prompt record IS a genuine human
# prompt -- checks that ITP itself (unlike IT) doesn't accidentally suppress
# a real nudge.
case_parked_row_with_genuine_last_prompt_is_still_nudged() {
    seed idle; W "$GENUINE"; "$ST" blocked "q?" >/dev/null
    local out; out="$(ITP 'please keep going' 'I will ask now.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block" and (.reason|test("SITREP-QUESTION"))' >/dev/null || { echo "    no nudge: '$out'"; return 1; }
    expect working/user/q/-/- end
}
case_plain_user_turn_without_marker_floors_blue_unnudged() {
    seed idle; W "$GENUINE"
    local out; out="$(IT 'It is 14:00.')"
    [ -z "$out" ] || { echo "    nudged a plain answer: '$out'"; return 1; }
    expect done/-/-/-/d end
}

# ---- effort vs exchange (2026-09-10): a long green turn is asked once ----
case_effort_user_turn_without_marker_gets_one_soft_nudge() {
    seed idle; W "$GENUINE"
    local out; out="$(ITX 10 60 'Fixed it; all green.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block" and (.reason|test("SITREP: ")) and (.reason|test("back-and-forth"))' >/dev/null || { echo "    no soft nudge: '$out'"; return 1; }
    expect working/user/-/-/- untouched || return 1
    out="$(ITX 10 60 'That was a step in our exchange.' true)"
    [ -z "$out" ] || { echo "    re-nudged in continuation: '$out'"; return 1; }
    expect done/-/-/-/d end
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
    expect done/-/-/-/d end
}
case_effort_machine_turn_is_not_nudged() {
    seed idle; W "$RELAY"
    local out; out="$(ITX 10 60 'peer handled')"
    [ -z "$out" ] || { echo "    nudged a machine turn: '$out'"; return 1; }
}
case_marker_variants_bold_after_and_heading_still_stamp() {
    seed idle; W "$GENUINE"
    IT $'**SITREP-WAITING**: the checks are rerunning\n\nOne job...' >/dev/null
    expect waiting/-/-/w/- bold-after && [ "$(summ)" = "the checks are rerunning" ] || { echo "    summary '$(summ)'"; return 1; }
    seed idle; W "$GENUINE"
    IT $'## SITREP: the loop is built\n\nThe chain...' >/dev/null
    expect done/-/-/-/d heading && [ "$(summ)" = "the loop is built" ] || { echo "    summary '$(summ)'"; return 1; }
}
case_turn_with_a_block_is_never_nudged_twice() {
    seed idle; W "$GENUINE"
    local out; out="$(ITX 12 600 $'SITREP-WAITING: the suite is running in `bg`\n\n- a bullet')"
    [ -z "$out" ] || { echo "    nudged a turn that had its block: '$out'"; return 1; }
    expect waiting/-/-/w/- stamped
}

# ---- artifact audit on a closing marker (2026-09-14) ----
# ITT TOOLSPEC TEXT [stop_hook_active]: like IT, but the turn also ran the
# tool_use calls in TOOLSPEC (comma-separated "NAME:::ARG" entries; ARG
# becomes .input.command for Bash, .input.file_path otherwise) before ending
# on the closing marker TEXT.
ITT() {
    local tr="$WORK/transcript.jsonl" spec="$1" i=0 entry name arg key
    local -a entries=()
    IFS=',' read -ra entries <<< "$spec"
    { jq -nc '{type:"user",message:{content:"go"}}'
      for entry in "${entries[@]}"; do
        name="${entry%%:::*}"; arg="${entry#*:::}"
        [ "$name" = Bash ] && key=command || key=file_path
        jq -nc --arg n "$name" --arg k "$key" --arg v "$arg" --arg id "t$i" \
            '{type:"assistant",message:{content:[{type:"tool_use",id:$id,name:$n,input:({($k):$v})}]}}'
        jq -nc --arg id "t$i" '{type:"user",message:{content:[{type:"tool_result",tool_use_id:$id,content:"ok"}]}}'
        i=$((i+1))
      done
      jq -nc --arg t "$2" '{type:"assistant",message:{content:[{type:"text",text:$t}]}}'; } > "$tr"
    jq -nc --arg p "$tr" --argjson a "${3:-false}" '{transcript_path:$p, stop_hook_active:$a}' | bash "$FLAT_BIN_DIR/comm-status-idle.sh"
}
# A stub `claude` on PATH stands in for the Haiku tier so these stay hermetic and free.
CLAUDE_STUB_DIR="$WORK/claude-stub"; mkdir -p "$CLAUDE_STUB_DIR"
cat > "$CLAUDE_STUB_DIR/claude" <<'STUB'
#!/usr/bin/env bash
cat >/dev/null
if [ -n "${SOT_TEST_CLAUDE_FINDINGS:-}" ]; then
    printf '%s' "$SOT_TEST_CLAUDE_FINDINGS"
else
    printf '%s' '{"findings":[]}'
fi
STUB
chmod +x "$CLAUDE_STUB_DIR/claude"
case_marker_artifact_audit_blocks_unsurfaced_result() {
    seed idle; W "$GENUINE"
    local out
    out="$(PATH="$CLAUDE_STUB_DIR:$PATH" SOT_TEST_CLAUDE_FINDINGS='{"findings":[{"kind":"artifact","message":"badge /tmp/brief.md"}]}' \
        ITT "Write:::/tmp/brief.md" $'SITREP: wrote the design brief\n\nThe plan is in /tmp/brief.md.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block" and (.reason|test("show-result")) and (.reason|test("never surfaced"))' >/dev/null \
        || { echo "    no artifact-audit block: '$out'"; return 1; }
    expect done/-/-/-/d state
}
case_marker_artifact_audit_clean_when_shown() {
    seed idle; W "$GENUINE"
    local out
    out="$(PATH="$CLAUDE_STUB_DIR:$PATH" \
        ITT "Write:::/tmp/brief.md,Read:::/tmp/brief.md,Bash:::show-result /tmp/brief.md" \
        $'SITREP: wrote and showed the design brief\n\nDone.')"
    [ -z "$out" ] || { echo "    unexpected block: '$out'"; return 1; }
    expect done/-/-/-/d state
}
case_marker_artifact_audit_skipped_in_continuation() {
    seed idle; W "$GENUINE"; "$ST" blocked "q?" >/dev/null
    local out
    out="$(PATH="$CLAUDE_STUB_DIR:$PATH" SOT_TEST_CLAUDE_FINDINGS='{"findings":[{"kind":"artifact","message":"badge it"}]}' \
        ITT "Write:::/tmp/brief.md" 'SITREP-QUESTION: which port?' true)"
    [ -z "$out" ] || { echo "    nudged a stop-hook continuation: '$out'"; return 1; }
    expect blocked/-/q/-/- state && [ "$(summ)" = "which port?" ]
}

# ---- races: the read-decide-write decides against the row as it is UNDER the lock ----
# Hold the registry lock, start the writer under test (it blocks on the lock;
# the barrier seam tells us it got there), commit a competing write, release,
# and assert the writer honoured the committed row rather than its pre-lock
# idea of it.
race() {  # SEED_FACTS_JSON COMPETING_JQ WANT
    seed_facts "$1"
    local barrier="$WORK/barrier.$$"; rm -f "$barrier"
    mkdir "$LOCKDIR" || return 1
    ( SOT_COMM_TEST_LOCK_BARRIER="$barrier" "$ST" stop ) &
    local pid=$! i=0
    while [ ! -e "$barrier" ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done
    [ -e "$barrier" ] || { rmdir "$LOCKDIR"; kill "$pid" 2>/dev/null; echo "    stop never reached the lock"; return 1; }
    jq --arg n "$NAME" "$2" "$REGISTRY" > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
    rmdir "$LOCKDIR"
    wait "$pid"
    expect "$3" after-race
}
case_race_done_committed_while_stop_waits_is_kept() {
    race '{}' '.agents[$n].done = true' done/-/-/-/d
}
case_race_machine_start_while_stop_waits_ends_gray() {
    race '{"floor":"user"}' '.agents[$n].floor = "machine"' idle/-/-/-/-
}

# ---- a failed mutation is a failed script ----
# A directory squatting on the tmp path makes the jq redirect fail; the row
# must be untouched and the exit non-zero, on both write paths.
case_failed_declaration_write_exits_nonzero() {
    seed idle; mkdir "$REGISTRY.tmp"
    local rc=0
    "$ST" working 2>/dev/null || rc=$?
    rmdir "$REGISTRY.tmp"
    [ "$rc" -ne 0 ] || { echo "    exit was 0"; return 1; }
    expect idle/-/-/-/- untouched
}
case_failed_prompt_write_exits_nonzero() {
    seed idle; W "$GENUINE"; "$ST" blocked "q?" >/dev/null; floor_now
    expect blocked/-/q/-/- setup || return 1
    mkdir "$REGISTRY.tmp"
    local rc=0
    COMM_STATUS_ORIGIN=machine "$ST" prompt 2>/dev/null || rc=$?
    rmdir "$REGISTRY.tmp"
    [ "$rc" -ne 0 ] || { echo "    exit was 0"; return 1; }
    expect blocked/-/q/-/- untouched
}

# ---- deaf-session warning (2026-09-15): comm-status-heartbeat.sh warns to
# stderr when a session's harness inbox watcher died but nothing told the
# session — see the hook's own header comment for the mechanism. ----
WATCH_MARKER="$SOT_COMM_HOME/state/$NAME.watch"
WARN_STAMP="$SOT_COMM_HOME/state/$NAME.watchwarn"
mkmarker() {  # PID [SESSION_ID] -- write a watcher marker in comm-watch.sh's own format
    mkdir -p "$(dirname "$WATCH_MARKER")"
    printf '%s\n%s\n' "$1" "${2:-}" > "$WATCH_MARKER"
}
rmmarker() { rm -f "$WATCH_MARKER" "$WARN_STAMP"; }
dead_pid() {  # a pid guaranteed not to be running: backgrounded, then reaped
    ( exit 0 ) & local p=$!
    wait "$p" 2>/dev/null
    echo "$p"
}
# HBW [SESSION_ID]: like HB, but runs with CLAUDE_CODE_SESSION_ID set (as a
# real Claude Code hook shell always has it), and leaves whatever the hook
# wrote to stderr in $HBW_ERR (stdout discarded, same as HB).
HBW() {
    rm -f "$SOT_COMM_HOME"/state/hb-*.tick 2>/dev/null
    HBW_ERR="$(printf '{"tool_name":"Bash"}' \
        | CLAUDE_CODE_SESSION_ID="${1:-sess-a}" bash "$FLAT_BIN_DIR/comm-status-heartbeat.sh" 2>&1 1>/dev/null)"
}
case_deaf_warns_on_dead_pid() {
    seed idle; rmmarker; mkmarker "$(dead_pid)"
    HBW
    [[ "$HBW_ERR" == *"no live inbox watcher for @$NAME"* ]] || { echo "    got '$HBW_ERR'"; return 1; }
}
case_deaf_warns_on_missing_marker() {
    seed idle; rmmarker
    HBW
    [[ "$HBW_ERR" == *"no live inbox watcher for @$NAME"* ]] || { echo "    got '$HBW_ERR'"; return 1; }
}
case_deaf_silent_while_watcher_alive() {
    seed idle; rmmarker
    sleep 30 & local p=$!
    mkmarker "$p" "sess-a"
    HBW
    kill "$p" 2>/dev/null; wait "$p" 2>/dev/null
    [ -z "$HBW_ERR" ] || { echo "    got '$HBW_ERR'"; return 1; }
}
case_deaf_silent_with_no_registry_row() {
    printf '{"agents":{}}\n' > "$REGISTRY"; rmmarker
    HBW
    [ -z "$HBW_ERR" ] || { echo "    got '$HBW_ERR'"; return 1; }
}
case_deaf_silent_without_session_id() {
    seed idle; rmmarker
    rm -f "$SOT_COMM_HOME"/state/hb-*.tick 2>/dev/null
    HBW_ERR="$(printf '{"tool_name":"Bash"}' | bash "$FLAT_BIN_DIR/comm-status-heartbeat.sh" 2>&1 1>/dev/null)"
    [ -z "$HBW_ERR" ] || { echo "    got '$HBW_ERR'"; return 1; }
}
case_deaf_warning_is_throttled() {
    seed idle; rmmarker
    HBW
    [ -n "$HBW_ERR" ] || { echo "    first call: expected a warning, got none"; return 1; }
    HBW
    [ -z "$HBW_ERR" ] || { echo "    second call inside the 10min window: got '$HBW_ERR'"; return 1; }
}
case_deaf_silent_for_subagent_sharing_parents_watcher() {
    # Same handle, marker alive, a DIFFERENT session id in both the env and
    # the marker's own second line — must stay silent: a lane shares its
    # parent's handle and must read the parent's live watcher as proof this
    # handle isn't deaf (see the hook's header comment for why there is
    # deliberately no session-id comparison here).
    seed idle; rmmarker
    sleep 30 & local p=$!
    mkmarker "$p" "sess-parent"
    HBW "sess-lane"
    kill "$p" 2>/dev/null; wait "$p" 2>/dev/null
    [ -z "$HBW_ERR" ] || { echo "    got '$HBW_ERR'"; return 1; }
}

check "reduction: a question with no floor is blocked, summary is the question" case_reduction_question_no_floor_is_blocked
check "reduction: a floor outranks question/waiting/done, summary is the note" case_reduction_floor_outranks_question_waiting_done
check "reduction: waiting outranks done, summary is the wait" case_reduction_waiting_outranks_done
check "reduction: done alone, summary is the note" case_reduction_done_alone
check "reduction: no facts is idle, summary is the note" case_reduction_nothing_is_idle
check "a genuine user turn ends blue" case_user_turn_ends_blue
check "a machine-started turn ends gray" case_machine_turn_ends_gray
check "a harness teammate report is a machine turn" case_teammate_report_is_a_machine_turn
check "a Stop hook send-back is a machine turn" case_stop_hook_sendback_is_a_machine_turn
check "blue survives a machine wake, gray at the next stop" case_blue_survives_machine_wake
check "blue is cleared by the next genuine prompt" case_blue_cleared_by_next_user_prompt
check "a question mid-turn is green, red once the turn stops" case_question_during_running_turn_is_green
check "a machine wake on red goes green; a plain answer returns red" case_machine_wake_on_red_goes_green_returns_at_stop
check "the user's answer clears the question and ends blue" case_human_answer_clears_question_ends_blue
check "a question outranks a wait; the wait returns once answered" case_red_over_purple
check "a wait survives a user turn and a machine turn" case_purple_survives_user_and_machine_turns
check "explicit working clears an open question and wait" case_explicit_working_clears_waiting_and_question
check "explicit idle clears an open question and wait" case_explicit_idle_clears_waiting_and_question
check "explicit done clears an open question and wait" case_explicit_done_clears_waiting_and_question
check "explicit done then stop stays done" case_explicit_done_then_stop_stays_done
check "explicit idle then stop stays idle" case_explicit_idle_then_stop_stays_idle
check "an AskUserQuestion answer inside the heartbeat throttle window still floors green" case_ask_user_question_within_throttle_window
check "a tool call on a floor-less row writes nothing" case_tool_call_on_floorless_row_changes_nothing
check "a tool call refreshes a stale stamp only, not a fresh one" case_tool_call_refreshes_old_stamp_only
check "headless child hooks stand down on SOT_COMM_HOOKS=off" case_headless_child_hooks_stand_down
check "a pre-amendment working row floors gray and drops its legacy keys" case_pre_field_working_row_floors_gray
check "a short exchange on a waiting row is never nudged" case_short_exchange_on_waiting_row_never_nudged
check "SITREP: stamps blue with the headline as summary" case_marker_done_stamps_blue_with_headline
check "SITREP-QUESTION: (bold-wrapped) stamps red with the question" case_marker_question_stamps_red
check "SITREP-WAITING: alone takes the next line and sets purple" case_marker_waiting_stamps_purple
check "a marker in a stop-hook continuation still stamps, no nudge" case_marker_in_continuation_still_stamps
check "a marker split across two assistant records still stamps, no nudge" case_marker_split_across_assistant_records_still_stamps
check "a marker present only in the Stop payload still stamps, no nudge" case_marker_only_in_stop_payload_still_stamps
check "a human turn parked blocked without a marker is nudged once, row untouched" case_parked_user_turn_blocked_without_marker_is_nudged_once
check "a human turn parked done without a marker is nudged once, row untouched" case_parked_user_turn_done_without_marker_is_nudged_once
check "a machine turn ending parked without a marker is not nudged" case_parked_machine_turn_without_marker_is_not_nudged
check "a parked row whose prompt hook was missed (machine-shaped transcript prompt) is not nudged, floor corrected" case_parked_row_with_missed_prompt_hook_is_not_nudged
check "a parked row with a genuine last prompt record is still nudged" case_parked_row_with_genuine_last_prompt_is_still_nudged
check "a plain human answer without a marker floors blue, no nudge" case_plain_user_turn_without_marker_floors_blue_unnudged
check "an effort turn (many tool calls) ending green with no marker gets one soft nudge, then floors" case_effort_user_turn_without_marker_gets_one_soft_nudge
check "a long quiet turn is an effort by duration" case_long_quiet_user_turn_is_an_effort_by_duration
check "a short exchange step is never nudged" case_short_exchange_turn_is_never_nudged
check "an effort on a machine wake is not nudged" case_effort_machine_turn_is_not_nudged
check "marker variants (bold closing before the colon, a heading) still stamp" case_marker_variants_bold_after_and_heading_still_stamp
check "a turn that already has its block is never nudged, whatever the block says" case_turn_with_a_block_is_never_nudged_twice
check "a marker naming an unsurfaced result is blocked by the artifact audit" case_marker_artifact_audit_blocks_unsurfaced_result
check "a marker whose result was read and show-result'd is not blocked" case_marker_artifact_audit_clean_when_shown
check "the artifact audit does not re-fire in a stop-hook continuation" case_marker_artifact_audit_skipped_in_continuation
check "race: a done committed while stop waits for the lock is kept" case_race_done_committed_while_stop_waits_is_kept
check "race: a machine start committed while stop waits ends gray, not blue" case_race_machine_start_while_stop_waits_ends_gray
check "a failed declaration write exits non-zero and leaves the row untouched" case_failed_declaration_write_exits_nonzero
check "a failed prompt-event write exits non-zero and leaves the row untouched" case_failed_prompt_write_exits_nonzero
check "deaf warning fires when the watcher marker's pid is dead" case_deaf_warns_on_dead_pid
check "deaf warning fires when the watcher marker is missing" case_deaf_warns_on_missing_marker
check "deaf warning stays silent while the watcher pid is alive" case_deaf_silent_while_watcher_alive
check "deaf warning stays silent with no registry row" case_deaf_silent_with_no_registry_row
check "deaf warning stays silent without CLAUDE_CODE_SESSION_ID" case_deaf_silent_without_session_id
check "deaf warning is throttled to once per window" case_deaf_warning_is_throttled
check "deaf warning stays silent for a subagent sharing its parent's live watcher" case_deaf_silent_for_subagent_sharing_parents_watcher
rmmarker

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
