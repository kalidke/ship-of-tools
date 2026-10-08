# Part of ../test-status-floor.sh: new mail at the turn boundary, closing markers, effort against exchange.
# The permission hook must leave the caller's open input untouched when disabled.
case_codex_hook_input_and_stamps() {
    local mode="$1" d="$WORK/codex-hook-$1" rc rest expected
    mkdir -p "$d/bin"
    cat > "$d/bin/comm-status.sh" <<'RECORDER'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SOT_COMM_HOME/calls"
RECORDER
    chmod +x "$d/bin/comm-status.sh"
    printf '{"tool_name":"fixture-tool"}\n' > "$d/input"
    exec 7< "$d/input"
    if [ "$mode" = unset ]; then
        env -u SOT_COMM_HOOKS SOT_COMM_HOME="$d" bash "$HOOKS_DIR/codex-status-blocked.sh" <&7 > "$d/out" 2> "$d/err"
    else
        SOT_COMM_HOOKS="$mode" SOT_COMM_HOME="$d" bash "$HOOKS_DIR/codex-status-blocked.sh" <&7 > "$d/out" 2> "$d/err"
    fi
    rc=$?; rest="$(cat <&7)"; exec 7<&-
    [ "$rc" -eq 0 ] && [ ! -s "$d/out" ] && [ ! -s "$d/err" ] || return 1
    if [ "$mode" = off ]; then
        [ "$rest" = '{"tool_name":"fixture-tool"}' ] || { echo "    off hook consumed caller input"; return 1; }
        [ ! -e "$d/calls" ] || { echo "    off hook called status"; return 1; }
    else
        expected=$'blocked codex permission request: fixture-tool\nstop'
        [ -z "$rest" ] && [ "$(cat "$d/calls")" = "$expected" ] || { echo "    $mode hook did not consume input and stamp blocked then stop"; return 1; }
    fi
}
case_codex_hook_off_without_home() {
    local rc
    env -u HOME -u SOT_COMM_HOME SOT_COMM_HOOKS=off bash "$HOOKS_DIR/codex-status-blocked.sh" < /dev/null > "$WORK/codex-no-home.out" 2> "$WORK/codex-no-home.err"
    rc=$?
    [ "$rc" -eq 0 ] && [ ! -s "$WORK/codex-no-home.err" ] || { echo "    off hook expanded HOME before standing down"; return 1; }
}
check "Codex off hook leaves input and status untouched" case_codex_hook_input_and_stamps off
check "Codex off hook exits before HOME expansion" case_codex_hook_off_without_home
check "Codex unset hook consumes input and stamps blocked then stop" case_codex_hook_input_and_stamps unset
check "Codex on hook consumes input and stamps blocked then stop" case_codex_hook_input_and_stamps on

# ---- new mail at the turn boundary (messaging ruling, 2026-09-26) ----
# The inbox IS the delivery path, so a turn must not end while directed mail
# sits unread: the hook reads this handle's own inbox and blocks with "run
# comm-poll.sh". Three facts make that safe to do on every Stop -- it fires,
# it fires once per pending batch, and a broadcast never fires it.
_mail_reset() {
    mkdir -p "$SOT_COMM_HOME/inbox" "$SOT_COMM_HOME/read" "$SOT_COMM_HOME/state"
    : > "$SOT_COMM_HOME/inbox/$NAME.jsonl"
    rm -f "${SOT_COMM_HOME:?}/read/$NAME.cursor" "${SOT_COMM_HOME:?}"/state/mail-*.tick
}
_mail_line() {  # TO [FROM] -> one inbox line, stamped now
    jq -nc --arg to "$1" --arg from "${2:-peer}" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        '{from:$from,to:$to,repo:"r",msg:"look at this",ts:$ts}' >> "$SOT_COMM_HOME/inbox/$NAME.jsonl"
}
case_pending_mail_blocks_naming_comm_poll() {
    seed idle; _mail_reset; _mail_line "$NAME"
    local out; out="$(IT 'all done.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block" and (.reason|test("comm-poll"))' >/dev/null \
        || { echo "    no mail block: '$out'"; return 1; }
}
case_second_stop_in_the_same_turn_does_not_block_again() {
    seed idle; _mail_reset; _mail_line "$NAME"
    local first second
    first="$(IT 'all done.')"
    [ -n "$first" ] || { echo "    the first stop did not block at all"; return 1; }
    second="$(IT 'all done.')"
    [ -z "$second" ] || { echo "    blocked twice for the same pending mail: '$second'"; return 1; }
}
case_broadcast_only_inbox_never_blocks() {
    seed idle; _mail_reset; _mail_line ""
    local out; out="$(IT 'all done.')"
    [ -z "$out" ] || { echo "    a broadcast blocked the stop: '$out'"; return 1; }
}
case_self_echo_never_blocks() {
    seed idle; _mail_reset; _mail_line "$NAME" "$NAME"
    local out; out="$(IT 'all done.')"
    [ -z "$out" ] || { echo "    a self-echo frame blocked the stop: '$out'"; return 1; }
}
# H1, H2: the hook counts by the wake's rule (sot_unread), so a line the wake
# never wakes for never holds a turn: one addressed to another handle, or one
# whose `to` is not a string.
case_a_line_to_another_handle_never_blocks() {
    seed idle; _mail_reset; _mail_line "someone-else"
    local out; out="$(IT 'all done.')"
    [ -z "$out" ] || { echo "    a line to another handle blocked the stop: '$out'"; return 1; }
}
case_a_line_with_a_numeric_to_never_blocks() {
    seed idle; _mail_reset
    jq -nc '{from:"peer",to:5,repo:"r",msg:"look at this"}' >> "$SOT_COMM_HOME/inbox/$NAME.jsonl"
    local out; out="$(IT 'all done.')"
    [ -z "$out" ] || { echo "    a line with a numeric to blocked the stop: '$out'"; return 1; }
}
case_offset_cursor_covering_the_inbox_never_blocks() {
    seed idle; _mail_reset; _mail_line "$NAME"
    # The cursor is a LINE OFFSET: one line filed, one line read.
    printf '%s' "1" > "$SOT_COMM_HOME/read/$NAME.cursor"
    local out; out="$(IT 'all done.')"
    [ -z "$out" ] || { echo "    mail the cursor already covers blocked the stop: '$out'"; return 1; }
}
case_same_second_frame_is_still_announced() {
    seed idle; _mail_reset
    # TWO frames stamped in the SAME second, one of them read. A timestamp
    # cursor cannot tell them apart -- every comparison was strictly-greater, so
    # the second frame was announced to nobody while its sender was told it had
    # landed. The offset can: line 2 is pending.
    local ts="2026-01-01T00:00:00Z" i
    for i in 1 2; do
        jq -nc --arg to "$NAME" --arg ts "$ts" --arg m "frame $i" \
            '{from:"peer",to:$to,repo:"r",msg:$m,ts:$ts}' >> "$SOT_COMM_HOME/inbox/$NAME.jsonl"
    done
    printf '%s' "1" > "$SOT_COMM_HOME/read/$NAME.cursor"
    local out; out="$(IT 'all done.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block" and (.reason|test("comm-poll"))' >/dev/null \
        || { echo "    the same-second frame was never announced: '$out'"; return 1; }
}
case_torn_line_does_not_silence_pending_mail() {
    seed idle; _mail_reset
    # A partial append ahead of the real message. Slurping the inbox as JSON
    # failed outright on one of these, which read as "no mail" at every turn end
    # from then on -- a handle permanently deaf while senders printed success.
    printf '{"from":"peer","to":"%s","msg":"half a li\n' "$NAME" >> "$SOT_COMM_HOME/inbox/$NAME.jsonl"
    _mail_line "$NAME"
    local out; out="$(IT 'all done.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block" and (.reason|test("comm-poll"))' >/dev/null \
        || { echo "    a torn line silenced the announcement: '$out'"; return 1; }
}
case_offset_past_the_end_still_announces() {
    seed idle; _mail_reset; _mail_line "$NAME"
    # The inbox was cleared/truncated by hand; the cursor still names the longer
    # file's offset. Left alone, this handle is never told about mail again.
    printf '%s' "99" > "$SOT_COMM_HOME/read/$NAME.cursor"
    local out; out="$(IT 'all done.')"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block"' >/dev/null \
        || { echo "    a stale offset silenced the announcement: '$out'"; return 1; }
}
case_unwritable_tick_fails_open() {
    seed idle; _mail_reset; _mail_line "$NAME"
    # With no tick there is no bound, and a filesystem that refuses this write
    # refuses comm-poll.sh's cursor write too -- so a block here would return at
    # every turn end with nothing the session could do about it. Fail open.
    chmod 000 "$SOT_COMM_HOME/state" 2>/dev/null
    local out; out="$(IT 'all done.')"
    chmod 755 "$SOT_COMM_HOME/state" 2>/dev/null
    [ -z "$out" ] || { echo "    blocked with no way to record the bound: '$out'"; return 1; }
}
case_mail_older_than_the_cursor_never_blocks() {
    seed idle; _mail_reset
    jq -nc '{from:"peer",to:"'"$NAME"'",repo:"r",msg:"old",ts:"2020-01-01T00:00:00Z"}' \
        >> "$SOT_COMM_HOME/inbox/$NAME.jsonl"
    printf '%s' "2026-01-01T00:00:00Z" > "$SOT_COMM_HOME/read/$NAME.cursor"
    local out; out="$(IT 'all done.')"
    [ -z "$out" ] || { echo "    mail the cursor already covers blocked the stop: '$out'"; return 1; }
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
