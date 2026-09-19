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
unset SOT_COMM_NAME COMM_STATUS_SOFT COMM_STATUS_ORIGIN CLAUDE_CODE_SESSION_ID
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
# hook), so the registry's turn_origin is whatever an EARLIER genuine prompt
# left it while the actual prompt record here is machine-shaped.
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
TEAMMATE='{"prompt":"Another Claude session sent a message:\\n<teammate-message teammate_id=x>done</teammate-message>"}'
STOPBACK='{"prompt":"Stop hook feedback:\\nYour row ends this turn as waiting"}'

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
case_teammate_report_is_a_machine_turn() { seed idle; W "$TEAMMATE"; expect working/machine/- start && I && expect idle/machine/- end; }
case_stop_hook_sendback_is_a_machine_turn() { seed idle; W "$STOPBACK"; expect working/machine/- start && I && expect idle/machine/- end; }
case_soft_done_holds_blue() { seed done; COMM_STATUS_SOFT=1 "$ST" done; expect done/-/- held; }
case_soft_done_holds_red() { seed blocked; COMM_STATUS_SOFT=1 "$ST" done; expect blocked/-/- held; }
# A live sticky marker: a HUMAN prompt paints green for the turn (marker kept),
# and the turn-end floor restores purple while the marker lives (2026-09-08).
case_sticky_waiting_survives_user_turn() { seed idle; "$ST" waiting "job"; W "$GENUINE"; expect working/user/sticky prompt && I && expect waiting/user/sticky end; }
case_explicit_idle_then_floor_is_gray() { seed idle; W "$GENUINE"; "$ST" idle; I; expect idle/user/- end; }
# A prompt paints green whoever sent it (owner, 2026-09-18: "green after the
# prompt until something else takes over"); a machine turn still ends gray,
# and a still-open question comes back red through the closing marker.
case_machine_wake_on_blue_goes_green() { seed idle; W "$GENUINE"; I; W "$RELAY"; expect working/machine/- wake && I && expect idle/machine/- end; }
# A question clears an earlier wait: red must not turn purple on the next
# tool call because of a marker from a job that has since landed.
case_explicit_blocked_clears_sticky() { seed idle; W "$GENUINE"; "$ST" waiting "job"; expect waiting/user/sticky wait && "$ST" blocked "q?"; expect blocked/user/- question && W "$GENUINE" && expect working/user/- answer; }
case_machine_wake_on_red_goes_green() { seed idle; W "$GENUINE"; "$ST" blocked "q?"; W "$RELAY"; expect working/machine/- wake && IT "SITREP-QUESTION: still which port?" && expect blocked/machine/- marker; }
case_explicit_working_in_machine_turn_on_red_ends_gray() { seed idle; W "$GENUINE"; "$ST" blocked "q?"; W "$RELAY"; "$ST" working "resuming"; expect working/machine/- explicit && I && expect idle/machine/- end; }
case_blocked_answered_ends_blue() { seed blocked; W "$GENUINE"; expect working/user/- answer && I && expect done/user/- end; }
# A headless claude launched by comm tooling (the auditor's tier-2 call) runs
# these hooks under the parent's identity; with SOT_COMM_HOOKS=off they stand
# down, so the parent's fresh red stays red through the child's prompt and Stop.
case_headless_child_hooks_stand_down() { seed idle; W "$GENUINE"; "$ST" blocked "q?"; SOT_COMM_HOOKS=off W "$GENUINE"; expect blocked/user/- child-prompt && SOT_COMM_HOOKS=off HB && expect blocked/user/- child-tool && SOT_COMM_HOOKS=off I && expect blocked/user/- child-stop; }
case_blue_cleared_by_next_prompt() { seed idle; "$ST" done "shipped"; expect done/-/- explicit && W "$GENUINE"; expect working/user/- next; }
case_machine_wake_on_sticky_then_explicit_working_ends_gray() { seed idle; W "$GENUINE"; "$ST" waiting "job"; W "$RELAY"; expect working/machine/sticky wake && "$ST" working "finishing"; expect working/machine/- explicit && I && expect idle/machine/- end; }
case_machine_wake_on_sticky_goes_green_and_stop_restores_purple() { seed idle; W "$GENUINE"; "$ST" waiting "job"; W "$RELAY"; expect working/machine/sticky wake && I && expect waiting/machine/sticky end; }
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
# 2026-09-14 live incident: a long reply's closing SITREP-WAITING: line was
# missed and nudged as "no closing marker" -- the reply streamed across two
# assistant transcript records and only the LAST record was ever searched.
case_marker_split_across_assistant_records_still_stamps() {
    seed idle; W "$GENUINE"
    local out
    out="$(IT2 $'Some analysis of the failure...\n\nSITREP-WAITING: the suite is rerunning in the background' \
        'One more thing: will report back once it lands.')"
    [ -z "$out" ] || { echo "    unexpected nudge: '$out'"; return 1; }
    expect waiting/user/sticky state && [ "$(summary)" = "the suite is rerunning in the background" ] || { echo "    summary '$(summary)'"; return 1; }
}
case_marker_only_in_stop_payload_still_stamps() {
    seed idle; W "$GENUINE"
    local out
    out="$(ITL 'Working on it.' 'SITREP-WAITING: the build is running')"
    [ -z "$out" ] || { echo "    unexpected nudge: '$out'"; return 1; }
    expect waiting/user/sticky state && [ "$(summary)" = "the build is running" ] || { echo "    summary '$(summary)'"; return 1; }
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
# 2026-09-15: the prompt hook missed this wake entirely (a harness-injected
# machine message that never fired UserPromptSubmit) -- the registry still
# says turn_origin=user from the earlier genuine prompt, but the transcript's
# own last prompt record is machine-shaped. The Stop hook must classify it
# itself: no nudge, and the registry's origin gets corrected too (so the
# floor paints gray, not blue, on a later turn).
case_parked_row_with_missed_prompt_hook_is_not_nudged() {
    seed idle; W "$GENUINE"; "$ST" waiting "job"
    local out
    out="$(ITP 'Another Claude session sent a message: <teammate-message teammate_id=x>report</teammate-message>' 'ack, noted.')"
    [ -z "$out" ] || { echo "    nudged despite a machine-shaped prompt record: '$out'"; return 1; }
    expect waiting/machine/sticky end
}
# Same shape, but the transcript's last prompt record IS a genuine human
# prompt -- the existing nudged-once case already covers this exact scenario
# (case_parked_user_turn_without_marker_is_nudged_once), so this only checks
# that ITP itself (unlike IT) doesn't accidentally suppress a real nudge.
case_parked_row_with_genuine_last_prompt_is_still_nudged() {
    seed idle; W "$GENUINE"; "$ST" waiting "job"
    local out; out="$(ITP 'please keep going' 'I launched the job and will report.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block" and (.reason|test("SITREP-WAITING"))' >/dev/null || { echo "    no nudge: '$out'"; return 1; }
    expect waiting/user/sticky end
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

# ---- artifact audit on a closing marker (2026-09-14, owner: "the hook
# should eval if something should be displayed in the preview" -- a session
# announced a design brief in a file but never showed it). A marker still
# stamps the row (below); this only covers the extra artifact-only auditor
# pass that now runs before the hook exits. A stub `claude` on PATH stands in
# for the Haiku tier so these stay hermetic and free.
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
# The hook locates the auditor next to ITSELF ($SELF_DIR/comm-turn-auditor.sh),
# which only resolves once hooks and scripts are deployed flat into one
# ~/.sot-comm/bin/ (update_comm) -- true in production, not in this checkout
# where hooks/ and core/scripts/ are separate directories. Run the hook from a
# flattened symlink dir so ITT exercises the auditor call the way it deploys.
FLAT_BIN_DIR="$WORK/flat-bin"; mkdir -p "$FLAT_BIN_DIR"
ln -s "$HOOKS_DIR/comm-status-idle.sh" "$FLAT_BIN_DIR/comm-status-idle.sh"
ln -s "$SCRIPTS_DIR/comm-turn-auditor.sh" "$FLAT_BIN_DIR/comm-turn-auditor.sh"
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
    expect done/user/- state
}
case_marker_artifact_audit_clean_when_shown() {
    seed idle; W "$GENUINE"
    local out
    out="$(PATH="$CLAUDE_STUB_DIR:$PATH" \
        ITT "Write:::/tmp/brief.md,Read:::/tmp/brief.md,Bash:::show-result /tmp/brief.md" \
        $'SITREP: wrote and showed the design brief\n\nDone.')"
    [ -z "$out" ] || { echo "    unexpected block: '$out'"; return 1; }
    expect done/user/- state
}
case_marker_artifact_audit_skipped_in_continuation() {
    seed idle; W "$GENUINE"; "$ST" blocked "q?"
    local out
    out="$(PATH="$CLAUDE_STUB_DIR:$PATH" SOT_TEST_CLAUDE_FINDINGS='{"findings":[{"kind":"artifact","message":"badge it"}]}' \
        ITT "Write:::/tmp/brief.md" 'SITREP-QUESTION: which port?' true)"
    [ -z "$out" ] || { echo "    nudged a stop-hook continuation: '$out'"; return 1; }
    expect blocked/user/- state && [ "$(summary)" = "which port?" ]
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

# ---- deaf-session warning (2026-09-15): comm-status-heartbeat.sh warns to
# stderr when a session's harness inbox watcher died but nothing told the
# session — see the hook's own header comment for the mechanism. ----
# The hook resolves NAME via $SELF_DIR/comm-context.sh (next to itself),
# which only resolves once hooks and scripts are deployed flat into one
# ~/.sot-comm/bin/ (update_comm) -- same reasoning as $FLAT_BIN_DIR above for
# comm-turn-auditor.sh. Reuse that dir here rather than a second one.
ln -sf "$HOOKS_DIR/comm-status-heartbeat.sh" "$FLAT_BIN_DIR/comm-status-heartbeat.sh"
ln -sf "$SCRIPTS_DIR/comm-context.sh" "$FLAT_BIN_DIR/comm-context.sh"
ln -sf "$SCRIPTS_DIR/comm-lib.sh" "$FLAT_BIN_DIR/comm-lib.sh"
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
# real Claude Code hook shell always has it), through the flattened bin dir
# so NAME actually resolves, and leaves whatever the hook wrote to stderr in
# $HBW_ERR (stdout discarded, same as HB).
HBW() {
    # The hook's 10 s early throttle (one tick per session) would silence
    # every call after the first inside one case run; each case is a fresh
    # tool call in its own right, so clear the tick first.
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

check "a genuine user turn ends blue" case_user_turn_ends_blue
check "a machine-started turn ends gray" case_machine_turn_ends_gray
check "a harness teammate report is a machine turn" case_teammate_report_is_a_machine_turn
check "a Stop hook send-back is a machine turn" case_stop_hook_sendback_is_a_machine_turn
check "the floor holds an explicit done" case_soft_done_holds_blue
check "the floor holds blocked" case_soft_done_holds_red
check "sticky waiting paints green through a user turn and returns to purple at the floor" case_sticky_waiting_survives_user_turn
check "explicit idle then the floor stays gray" case_explicit_idle_then_floor_is_gray
check "a machine wake on a blue row goes green, gray at Stop" case_machine_wake_on_blue_goes_green
check "a machine wake on a red row goes green; the closing marker restores red" case_machine_wake_on_red_goes_green
check "an explicit blocked clears an earlier sticky wait; the answer turn stays green" case_explicit_blocked_clears_sticky
check "explicit working inside a machine turn on a red row ends gray (no stale origin)" case_explicit_working_in_machine_turn_on_red_ends_gray
check "blocked answered by the user ends blue" case_blocked_answered_ends_blue
check "headless child hooks stand down on SOT_COMM_HOOKS=off" case_headless_child_hooks_stand_down
check "explicit done is cleared by the next genuine prompt" case_blue_cleared_by_next_prompt
check "machine wake on sticky purple then explicit working ends gray" case_machine_wake_on_sticky_then_explicit_working_ends_gray
check "a machine wake on sticky purple goes green; Stop restores purple" case_machine_wake_on_sticky_goes_green_and_stop_restores_purple
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
check "a marker split across two assistant records still stamps, no nudge" case_marker_split_across_assistant_records_still_stamps
check "a marker present only in the Stop payload still stamps, no nudge" case_marker_only_in_stop_payload_still_stamps
check "a human turn ending parked without a marker is nudged once, row untouched" case_parked_user_turn_without_marker_is_nudged_once
check "a machine turn ending parked without a marker is not nudged" case_parked_machine_turn_without_marker_is_not_nudged
check "a parked row whose prompt hook was missed (machine-shaped transcript prompt) is not nudged, origin corrected" case_parked_row_with_missed_prompt_hook_is_not_nudged
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
check "race: a done committed while the floor waits for the lock is kept" case_race_done_committed_while_floor_waits_is_kept
check "race: a machine start committed while the floor waits ends gray, not blue" case_race_machine_start_while_floor_waits_ends_gray
check "a failed state write exits non-zero and leaves the row untouched" case_failed_state_write_exits_nonzero
check "a failed origin-only write exits non-zero and leaves the row untouched" case_failed_origin_write_exits_nonzero
check "the Stop hook sends soft done only to a floor-aware comm-status.sh" case_stop_hook_sends_done_only_to_a_floor_aware_script
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
