# Part of ../test-status-floor.sh: the artifact audit, held marker turns, races and failed mutations.
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
    expect working/user/-/-/d state
}
# A marker turn whose artifact audit blocks has not ended: the row keeps its
# floor (and the Stop hook's `stop_at` mark) until the Stop that lets it end,
# which sends `stop` and clears both.
case_marker_audit_block_keeps_the_turn_until_its_last_stop() {
    seed idle; W "$GENUINE"
    local out marker=$'SITREP: wrote the third brief\n\nThe plan is in /tmp/brief3.md.'
    out="$(PATH="$CLAUDE_STUB_DIR:$PATH" SOT_TEST_CLAUDE_FINDINGS='{"findings":[{"kind":"artifact","message":"badge /tmp/brief3.md"}]}' \
        ITT "Write:::/tmp/brief3.md" "$marker")"
    printf '%s' "$out" | jq -e '.decision=="block"' >/dev/null || { echo "    no audit block: '$out'"; return 1; }
    expect working/user/-/-/d held || return 1
    [[ "$(stop_mark)" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:]{8}Z$ ]] || { echo "    no stop_at after the block: '$(stop_mark)'"; return 1; }
    out="$(PATH="$CLAUDE_STUB_DIR:$PATH" ITT "Write:::/tmp/brief3.md" "$marker" true)"
    [ -z "$out" ] || { echo "    the continuation blocked: '$out'"; return 1; }
    expect done/-/-/-/d ended || return 1
    [ -z "$(stop_mark)" ] || { echo "    stop_at survived the ending Stop: '$(stop_mark)'"; return 1; }
}
case_stop_deletes_the_stop_mark() {
    seed_facts '{"floor":"user","stop_at":"2026-09-08T00:00:00Z"}'
    "$ST" stop >/dev/null
    [ -z "$(stop_mark)" ] || { echo "    stop_at survived stop: '$(stop_mark)'"; return 1; }
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

# ---- the inbox read runs before the closing marker (B1 fix-up 7b) ----
# A marker turn does not end on unread mail: the read and the mail gate come
# before the marker branch, so a turn that closes with a marker while directed
# mail waits blocks with the same "run comm-poll.sh" text, neither stamped from
# its marker nor floored; the turn end that passes stamps. With no unread mail
# a marker turn is exactly as before.
MAIL_BLOCK="{\"decision\":\"block\",\"reason\":\"New sot-comm mail for @$NAME — run comm-poll.sh now, act on it, then end the turn.\"}"
# A Stop that blocks leaves the Stop hook's own `stop_at` mark, so the row is compared without it.
row_all() { jq -c --arg n "$NAME" '.agents[$n] | del(.stop_at)' "$REGISTRY"; }
stop_mark() { jq -r --arg n "$NAME" '.agents[$n].stop_at // ""' "$REGISTRY"; }
poll_mail() { "$SCRIPTS_DIR/comm-poll.sh" >/dev/null 2>&1; }
# ---- a held marker turn keeps its marker (B1 fix-up 8) ----
# The turn end that passes stamps from the LAST marker anywhere in the logical
# turn: the hook's own held-turn notices do not start a new one. The shape is a
# live transcript's (2026-09-30): a block comes back as an isMeta `user` record
# with the opening prompt's promptId and "Stop hook feedback:\n" plus the
# reason, byte for byte. LT_NEW PROMPT starts the turn (and a fresh session:
# no recorded feedback), LT_PROMPT appends a real prompt, LT_REPLY a reply,
# LT_BLOCKED OUT the feedback for the block OUT the hook itself printed, and
# LT_STOP [stop_hook_active] runs the Stop hook over it. Every second or later
# Stop runs with stop_hook_active true.
LT="$WORK/logical.jsonl"; LT_ID=0
LT_PROMPT() {
    LT_ID=$((LT_ID + 1))
    jq -nc --arg p "$1" --arg id "prompt-$LT_ID" '{type:"user",promptId:$id,message:{role:"user",content:$p}}' >> "$LT"
}
LT_NEW() { rm -f "${SOT_COMM_HOME:?}"/state/stop-feedback-*.jsonl; : > "$LT"; LT_PROMPT "$1"; }
LT_REPLY() { jq -nc --arg t "$1" '{type:"assistant",message:{content:[{type:"text",text:$t}]}}' >> "$LT"; }
LT_FEEDBACK() {
    jq -nc --arg r "$1" --arg id "prompt-$LT_ID" \
        '{type:"user",isMeta:true,promptId:$id,message:{role:"user",content:("Stop hook feedback:\n" + $r)}}' >> "$LT"
}
LT_BLOCKED() { LT_FEEDBACK "$(printf '%s' "$1" | jq -r '.reason')"; }
LT_STOP() { jq -nc --arg p "$LT" --argjson a "${1:-false}" '{transcript_path:$p, stop_hook_active:$a}' | bash "$HOOKS_DIR/comm-status-idle.sh"; }
# held_marker_turn REPLY — REPLY closes a human turn while mail waits: the hook
# blocks, unstamped; the model polls and replies "nothing for me"; that turn
# end passes.
held_marker_turn() {
    seed idle; W "$GENUINE"; _mail_reset; _mail_line "$NAME"
    LT_NEW "please do the thing"; LT_REPLY "$1"
    local before out; before="$(row_all)"
    out="$(LT_STOP)"
    [ "$out" = "$MAIL_BLOCK" ] || { echo "    the marker turn did not block on the mail: '$out'"; return 1; }
    [ "$(row_all)" = "$before" ] || { echo "    the blocked marker turn changed the row: $before -> $(row_all)"; return 1; }
    poll_mail || { echo "    comm-poll.sh failed"; return 1; }
    LT_BLOCKED "$out"; LT_REPLY 'nothing for me'
    out="$(LT_STOP true)"
    [ -z "$out" ] || { echo "    the turn end after the poll still blocked: '$out'"; return 1; }
    fb_gone || return 1
}
# The passing Stop removes the session's feedback record (the hook's EXIT trap).
fb_gone() {
    local left; left="$(ls "${SOT_COMM_HOME:?}"/state/stop-feedback-* 2>/dev/null)"
    [ -z "$left" ] || { echo "    the passing Stop left its feedback record: $left"; return 1; }
}
case_a_held_question_turn_ends_red_with_its_question() {
    held_marker_turn $'SITREP-QUESTION: which port?\n\nContext...' || return 1
    expect blocked/-/q/-/- state && [ "$(summ)" = "which port?" ] || { echo "    summary '$(summ)'"; return 1; }
}
case_a_held_waiting_or_done_turn_ends_as_its_marker() {
    held_marker_turn 'SITREP-WAITING: the build' || return 1
    expect waiting/-/-/w/- waiting && [ "$(summ)" = "the build" ] || { echo "    summary '$(summ)'"; return 1; }
    held_marker_turn $'SITREP: finished the port\n\nThe chain...' || return 1
    expect done/-/-/-/d done && [ "$(summ)" = "finished the port" ] || { echo "    summary '$(summ)'"; return 1; }
}
case_the_last_marker_in_a_held_turn_wins() {
    seed idle; W "$GENUINE"; _mail_reset; _mail_line "$NAME"
    LT_NEW "please do the thing"; LT_REPLY 'SITREP: the first word'
    local out; out="$(LT_STOP)"
    [ "$out" = "$MAIL_BLOCK" ] || { echo "    no mail block: '$out'"; return 1; }
    poll_mail; LT_BLOCKED "$out"; LT_REPLY 'SITREP-WAITING: the second word'
    out="$(LT_STOP true)"
    [ -z "$out" ] || { echo "    the turn end after the poll still blocked: '$out'"; return 1; }
    expect waiting/-/-/w/- state && [ "$(summ)" = "the second word" ] || { echo "    summary '$(summ)'"; return 1; }
}
case_a_turn_held_twice_keeps_its_first_marker() {
    seed idle; W "$GENUINE"; _mail_reset; _mail_line "$NAME"
    LT_NEW "please do the thing"; LT_REPLY 'SITREP-QUESTION: which port?'
    local out; out="$(LT_STOP)"
    [ "$out" = "$MAIL_BLOCK" ] || { echo "    no first block: '$out'"; return 1; }
    LT_BLOCKED "$out"; LT_REPLY 'looking'; _mail_line "$NAME"
    out="$(LT_STOP true)"
    [ "$out" = "$MAIL_BLOCK" ] || { echo "    no second block for the new mail: '$out'"; return 1; }
    poll_mail; LT_BLOCKED "$out"; LT_REPLY 'nothing for me'
    out="$(LT_STOP true)"
    [ -z "$out" ] || { echo "    the third turn end still blocked: '$out'"; return 1; }
    expect blocked/-/q/-/- state && [ "$(summ)" = "which port?" ] || { echo "    summary '$(summ)'"; return 1; }
}
case_a_real_prompt_after_a_marker_turn_starts_a_new_turn() {
    held_marker_turn 'SITREP-QUESTION: which port?' || return 1
    expect blocked/-/q/-/- asked || return 1
    local out
    W "$GENUINE"; LT_PROMPT "port 8080"; LT_REPLY 'It listens on 8080 now.'
    out="$(LT_STOP)"
    [ -z "$out" ] || { echo "    the answered turn blocked: '$out'"; return 1; }
    expect done/-/-/-/d answered && fb_gone
}
case_feedback_the_hook_did_not_record_reads_as_a_prompt() {
    seed idle; W "$GENUINE"; _mail_reset; _mail_line "$NAME"
    LT_NEW "please do the thing"; LT_REPLY 'SITREP-QUESTION: which port?'
    local out; out="$(LT_STOP)"
    [ "$out" = "$MAIL_BLOCK" ] || { echo "    no mail block: '$out'"; return 1; }
    poll_mail; LT_FEEDBACK "$(printf '%s' "$out" | jq -r '.reason') (and another hook's words)"; LT_REPLY 'nothing for me'
    out="$(LT_STOP true)"
    [ -z "$out" ] || { echo "    the turn end blocked: '$out'"; return 1; }
    expect done/-/-/-/d "a loosely matching feedback kept the marker"
}
case_feedback_without_ismeta_reads_as_a_prompt() {
    seed idle; W "$GENUINE"; _mail_reset; _mail_line "$NAME"
    LT_NEW "please do the thing"; LT_REPLY 'SITREP-QUESTION: which port?'
    local out; out="$(LT_STOP)"
    [ "$out" = "$MAIL_BLOCK" ] || { echo "    no mail block: '$out'"; return 1; }
    poll_mail
    # the exact recorded text, pasted by a person: no isMeta
    jq -nc --arg r "$(printf '%s' "$out" | jq -r '.reason')" \
        '{type:"user",message:{role:"user",content:("Stop hook feedback:\n" + $r)}}' >> "$LT"
    LT_REPLY 'nothing for me'
    out="$(LT_STOP true)"
    [ -z "$out" ] || { echo "    the turn end blocked: '$out'"; return 1; }
    expect done/-/-/-/d "the pasted text was taken for the hook's own feedback"
}
case_mail_filed_mid_turn_blocks_the_marker_end() {
    seed idle; _mail_reset; _mail_line "$NAME"; poll_mail
    W "$GENUINE"; _mail_line "$NAME"
    local before out; before="$(row_all)"
    out="$(IT 'SITREP-WAITING: the build')"
    [ "$out" = "$MAIL_BLOCK" ] && [ "$(row_all)" = "$before" ] || { echo "    '$out' $before -> $(row_all)"; return 1; }
}
case_marker_turn_with_read_mail_is_unchanged() {
    seed idle; W "$GENUINE"; _mail_reset; _mail_line "$NAME"; poll_mail
    local out rc=0
    out="$(IT $'SITREP: nothing waiting\n\nDone.')" || rc=$?
    [ "$rc" -eq 0 ] && [ -z "$out" ] || { echo "    rc $rc, '$out'"; return 1; }
    expect done/-/-/-/d state && [ "$(summ)" = "nothing waiting" ] || { echo "    summary '$(summ)'"; return 1; }
}
case_marker_audit_block_with_read_mail_is_byte_identical() {
    seed idle; W "$GENUINE"; _mail_reset; _mail_line "$NAME"; poll_mail
    local out
    out="$(PATH="$CLAUDE_STUB_DIR:$PATH" SOT_TEST_CLAUDE_FINDINGS='{"findings":[{"kind":"artifact","message":"badge /tmp/brief2.md"}]}' \
        ITT "Write:::/tmp/brief2.md" $'SITREP: wrote the second brief\n\nThe plan is in /tmp/brief2.md.')"
    [ "$out" = '{"decision":"block","reason":"Your closing block names a result that was never surfaced: [artifact] badge /tmp/brief2.md -- badge it now via the show-result skill (show-result <path>), then end the turn. Your row is already stamped from the marker -- do not write a second sitrep block."}' ] \
        || { echo "    '$out'"; return 1; }
    expect working/user/-/-/d state && [ "$(summ)" = "wrote the second brief" ] || { echo "    summary '$(summ)'"; return 1; }
}

# ---- races: the read-decide-write decides against the row as it is UNDER the lock ----
# Hold the registry lock, start the writer under test (it blocks on the lock;
# the barrier seam tells us it got there), commit a competing write, release,
# and assert the writer honoured the committed row rather than its pre-lock
# idea of it.
race() {  # SEED_FACTS_JSON COMPETING_JQ WANT
    seed_facts "$1"
    local barrier="$WORK/barrier.$$"; rm -f "${barrier:?}"
    mkdir "${_SOT_REG_LOCK:?}" || return 1
    ( SOT_COMM_TEST_LOCK_BARRIER="$barrier" "$ST" stop ) &
    local pid=$!
    await test -e "$barrier" || { rmdir "${_SOT_REG_LOCK:?}"; kill "$pid" 2>/dev/null; echo "    stop never reached the lock"; return 1; }
    jq --arg n "$NAME" "$2" "$REGISTRY" > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
    rmdir "${_SOT_REG_LOCK:?}"
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
