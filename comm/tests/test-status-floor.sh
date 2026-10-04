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
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/../work_state/hooks" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-status-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
trap 'rm -rf "${WORK:?}"' EXIT

export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
SCRIPT_DIR="$SCRIPTS_DIR"
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
# B: the PreToolUse AskUserQuestion hook (sends blocked with the question, then stop). Carries
# a fixed tool_use_id so a paired HBQ call in the SAME test can consume the
# marker it drops; a bare HBQ with no preceding B for that id is exactly the
# "foreign dialog, no marker" case (see the fix-1 cases below).
B() { printf '{"tool_name":"AskUserQuestion","tool_use_id":"askq-test","tool_input":{"questions":[{"question":"A question?"}]}}' | bash "$HOOKS_DIR/comm-status-blocked.sh"; }
# HB: a heartbeat tool call (PostToolUse, tool_name=Bash). HBQ: the
# AskUserQuestion ANSWER's PostToolUse. Both route through FLAT_BIN_DIR so
# the hook's own NAME resolution (SELF_DIR/comm-context.sh) succeeds. HB
# clears the 10s early-throttle tick first: each HB call in a test is its
# own tool call in its own right, not a repeat within one live turn. HBQ
# does NOT clear it — the answer branch runs before that throttle check
# (review finding 2026-09-19), so a call must prove it fires even with a
# fresh tick left over from an earlier HB/B call in the same test. HBQ's
# tool_use_id matches B's default, so the two pair up when called together;
# called alone it finds no marker and answers nothing (fix 1).
HB() {
    rm -f "${SOT_COMM_HOME:?}"/state/hb-*.tick 2>/dev/null
    printf '{"tool_name":"Bash"}' | bash "$FLAT_BIN_DIR/comm-status-heartbeat.sh"
}
HBQ() {
    printf '{"tool_name":"AskUserQuestion","tool_use_id":"askq-test"}' | bash "$FLAT_BIN_DIR/comm-status-heartbeat.sh"
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

. "$(dirname "${BASH_SOURCE[0]}")/status_floor/reduction.sh"
. "$(dirname "${BASH_SOURCE[0]}")/status_floor/markers.sh"
. "$(dirname "${BASH_SOURCE[0]}")/status_floor/audit_and_races.sh"

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
check "declared text supersedes an old note (blocked and waiting)" case_declared_text_supersedes_an_old_note
check "a declaration without text keeps the fact's own text (blocked and waiting)" case_a_blank_declaration_keeps_its_own_text
check "waiting clears an open question silently" case_waiting_clears_an_open_question
check "waiting with no pending question prints nothing" case_waiting_without_a_question_is_silent
check "a bare blocked or waiting over newline-only text is refused" case_newline_only_text_is_refused
check "waiting is silent and refusable under a CRLF jq" case_waiting_under_a_crlf_jq
check "a blocked or waiting with no text is refused, nothing written" case_textless_blocked_or_waiting_is_refused
check "the AskUserQuestion hook carries the question text" case_askq_hook_carries_the_question
check "a SITREP-QUESTION: with no text leaves a running floor and no question" case_marker_question_without_text_leaves_the_floor
check "a stamp failure other than the refusal still floors the turn" case_non_refusal_stamp_failure_still_floors
check "a refused stamp leaves no temp file" case_refused_stamp_leaves_no_temp_file
check "the user's answer clears the question and ends blue" case_human_answer_clears_question_ends_blue
check "a question outranks a wait; the wait returns once answered" case_red_over_purple
check "a wait survives a user turn and a machine turn" case_purple_survives_user_and_machine_turns
check "explicit working clears an open question and wait" case_explicit_working_clears_waiting_and_question
check "explicit idle clears an open question and wait" case_explicit_idle_clears_waiting_and_question
check "explicit done clears an open question and wait" case_explicit_done_clears_waiting_and_question
check "explicit done then stop stays done" case_explicit_done_then_stop_stays_done
check "explicit idle then stop stays idle" case_explicit_idle_then_stop_stays_idle
check "an AskUserQuestion answer inside the heartbeat throttle window still floors green" case_ask_user_question_within_throttle_window
check "a parked question survives a foreign AskUserQuestion answer with no marker" case_parked_question_survives_a_foreign_askuserquestion_answer
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

# A stable tick key for the mail cases: the block is bounded per SESSION, and
# each IT call is its own short-lived process, so $PPID would differ per call.
export CLAUDE_CODE_SESSION_ID=mail-turn
check "pending directed mail blocks the stop, naming comm-poll" case_pending_mail_blocks_naming_comm_poll
check "a second stop for the same pending mail does not block again" case_second_stop_in_the_same_turn_does_not_block_again
check "a broadcast-only inbox never blocks the stop" case_broadcast_only_inbox_never_blocks
check "a self-echo frame never blocks the stop" case_self_echo_never_blocks
check "an offset cursor covering the inbox never blocks the stop" case_offset_cursor_covering_the_inbox_never_blocks
check "a frame filed in the same second as one already read is still announced" case_same_second_frame_is_still_announced
check "a torn inbox line does not silence pending mail" case_torn_line_does_not_silence_pending_mail
check "an offset past the end of the inbox still announces" case_offset_past_the_end_still_announces
check "an unwritable tick fails open instead of blocking every turn" case_unwritable_tick_fails_open
check "a legacy timestamp cursor that covers the inbox never blocks the stop" case_mail_older_than_the_cursor_never_blocks
unset CLAUDE_CODE_SESSION_ID
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
check "(a) a question turn held on mail ends red with its question" case_a_held_question_turn_ends_red_with_its_question
check "(b) a waiting or done turn held on mail ends as its marker says" case_a_held_waiting_or_done_turn_ends_as_its_marker
check "(c) the last marker in a held turn wins" case_the_last_marker_in_a_held_turn_wins
check "(d) a turn held twice keeps its first marker" case_a_turn_held_twice_keeps_its_first_marker
check "(e) a real prompt after a marker turn starts a new turn" case_a_real_prompt_after_a_marker_turn_starts_a_new_turn
check "(f) a feedback record the hook did not record reads as a prompt" case_feedback_the_hook_did_not_record_reads_as_a_prompt
check "(g) the recorded text without isMeta reads as a prompt" case_feedback_without_ismeta_reads_as_a_prompt
check "mail filed mid-turn blocks that turn's marker end" case_mail_filed_mid_turn_blocks_the_marker_end
check "a marker turn's audit block keeps the turn and its stop mark until the Stop that ends it" case_marker_audit_block_keeps_the_turn_until_its_last_stop
check "stop deletes the Stop hook's stop_at mark" case_stop_deletes_the_stop_mark
check "a marker turn with its mail read prints nothing and stamps as before" case_marker_turn_with_read_mail_is_unchanged
check "a marker turn's artifact-audit block is byte-identical with its mail read" case_marker_audit_block_with_read_mail_is_byte_identical
check "race: a done committed while stop waits for the lock is kept" case_race_done_committed_while_stop_waits_is_kept
check "race: a machine start committed while stop waits ends gray, not blue" case_race_machine_start_while_stop_waits_ends_gray
check "a failed declaration write exits non-zero and leaves the row untouched" case_failed_declaration_write_exits_nonzero
check "a failed prompt-event write exits non-zero and leaves the row untouched" case_failed_prompt_write_exits_nonzero
rmmarker

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
