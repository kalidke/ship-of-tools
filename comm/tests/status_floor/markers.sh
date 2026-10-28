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
    elif [ "$mode" = crlf ]; then
        # A native Windows jq ends each line with CR: the stamp must not carry it.
        mkdir -p "$d/crlf"
        printf '#!/usr/bin/env bash\n"%s" "$@" | sed "s/$/\\r/"\n' "$(command -v jq)" > "$d/crlf/jq"; chmod +x "$d/crlf/jq"
        PATH="$d/crlf:$PATH" SOT_COMM_HOME="$d" bash "$HOOKS_DIR/codex-status-blocked.sh" <&7 > "$d/out" 2> "$d/err"
    else
        SOT_COMM_HOOKS="$mode" SOT_COMM_HOME="$d" bash "$HOOKS_DIR/codex-status-blocked.sh" <&7 > "$d/out" 2> "$d/err"
    fi
    rc=$?; rest="$(cat <&7)"; exec 7<&-
    [ "$rc" -eq 0 ] && [ ! -s "$d/out" ] && [ ! -s "$d/err" ] || return 1
    if [ "$mode" = off ]; then
        [ "$rest" = '{"tool_name":"fixture-tool"}' ] || { echo "    off hook consumed caller input"; return 1; }
        [ ! -e "$d/calls" ] || { echo "    off hook called status"; return 1; }
    else
        expected='dialog codex permission request: fixture-tool'
        [ -z "$rest" ] && [ "$(cat "$d/calls")" = "$expected" ] || { echo "    $mode hook did not consume input and stamp the dialog"; return 1; }
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
check "Codex unset hook consumes input and stamps the dialog" case_codex_hook_input_and_stamps unset
check "Codex on hook consumes input and stamps the dialog" case_codex_hook_input_and_stamps on
check "Codex hook under a CRLF jq stamps no CR" case_codex_hook_input_and_stamps crlf

# ---- new mail at the turn boundary (2026-09-26) ----
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
# H1, H2: the hook counts by the wake's rule (sotd comm unread), so a line the wake
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
    local question='which box did you press W on? Options: (a) this one; (b) another one. Recommend (a).'
    IT "**SITREP-QUESTION: $question**"$'\n\nContext...' >/dev/null
    expect blocked/-/q/-/- state && [ "$(summ)" = "$question" ] || { echo "    summary '$(summ)'"; return 1; }
}
# The shape refusal keeps every row fact (apart from the Stop mark). These
# cases observe the real hook and registry, including a correction or bounded
# continuation after the refusal; each catches the old unconditional stamp.
question_shape_block() {
    [ -n "$1" ] && printf '%s' "$1" | jq -e '.decision=="block" and (.reason | contains("SITREP-QUESTION: <question> Irreversible: <what happens>. Options: (a) ...; (b) .... Recommend (a). Page: <absolute path>"))' >/dev/null \
        || { echo "    no question-shape block: '$1'"; return 1; }
}
case_incomplete_question_marker_is_refused() {
    seed idle; W "$GENUINE"
    local before out; before="$(row_all)"
    out="$(IT "SITREP-QUESTION: $1")"
    question_shape_block "$out" || return 1
    [ "$(row_all)" = "$before" ] || { echo "    the deficient marker changed the row"; return 1; }
}
hold_bare_question() {
    seed idle; W "$GENUINE"
    LT_NEW 'please choose a port'
    LT_REPLY $'SITREP-QUESTION: which port?\n\nOptions: (a) 8080; (b) 8081. Recommend (a).'
    local before out; before="$(row_all)"
    out="$(LT_STOP)"
    question_shape_block "$out" || return 1
    [ "$(row_all)" = "$before" ] || { echo "    the bare marker changed the row"; return 1; }
    LT_BLOCKED "$out"
}
case_bare_question_marker_is_refused() {
    hold_bare_question || return 1
    expect working/user/-/-/- unstamped
}
case_deficient_question_keeps_the_declared_question() {
    seed idle; W "$GENUINE"; "$ST" blocked "$PORT_QUESTION" >/dev/null
    local before out; before="$(row_all)"
    out="$(IT 'SITREP-QUESTION: which port?')"
    question_shape_block "$out" || return 1
    [ "$(row_all)" = "$before" ] || { echo "    the full declaration was overwritten"; return 1; }
}
case_question_grammar_boundaries_are_refused() {
    local question
    for question in \
        'which port? Options: (a) 8080. Recommend (a). (b) 8081.' \
        'which port? Recommend (a). Options: (a) 8080; (b) 8081.' \
        'which port? Options: (a) 8080; (b) 8081. Recommended.' \
        'which port? Options: (a) 8080; (b) 8081. Page: /project/Recommend(a)' \
        'which port? Page: /project/Options:(a)(b) Recommend (a).' \
        'which port? Options: (a) 8080. Recommend (a). Options: (b) 8081.'; do
        case_incomplete_question_marker_is_refused "$question" || return 1
    done
}
case_corrected_question_stamps_the_whole_line() {
    local question out
    # The second is an older line: its retired Pane only: clause still passes and stamps whole.
    for question in "$PORT_QUESTION" \
        'Land it? Irreversible: the change is merged. Pane only: approve here. Options: (a) merge; (b) wait. Recommend (a). Page: /project/decision.html' \
        'which port? Page: /project/decision.html. Options: (a) 8080; (b) 8081. Recommended: b' \
        'which port? Options: (A) 8080; (B) 8081. Recommend: (B).' \
        'which port? Options: (a) 8080; (b) 8081. Recommend: 7; Recommended b'; do
        hold_bare_question || return 1
        out="$(LT_STOP true)"
        [ -z "$out" ] || { echo "    the continuation blocked: '$out'"; return 1; }
        W "$GENUINE"; LT_NEW 'please choose a port with the full question line'
        LT_REPLY "**SITREP-QUESTION: $question**"$'\n\nContext to answer cold.'
        out="$(LT_STOP)"
        [ -z "$out" ] || { echo "    the corrected marker blocked: '$out'"; return 1; }
        expect blocked/-/q/-/- stamped || return 1
        jq -e --arg n "$NAME" --arg q "$question" '.agents[$n] | .question == $q and .summary == $q' "$REGISTRY" >/dev/null \
            || { echo "    the complete question was not kept: '$(summ)'"; return 1; }
    done
}
case_bare_question_continuation_stamps() {
    hold_bare_question || return 1
    LT_REPLY 'SITREP-QUESTION: which port?'
    local out; out="$(LT_STOP true)"
    [ -z "$out" ] || { echo "    the continuation blocked: '$out'"; return 1; }
    expect blocked/-/q/-/- stamped && [ "$(summ)" = 'which port?' ]
}
# A question that owes a decision page (rows' question::page_owed: Irreversible, or past two sentences) is not stamped
# at a turn's first stop: the hook asks for the page and keeps the row; its continuation stamps the line as written.
IRREV_QUESTION='Merge it? Irreversible: main moves. Options: (a) merge; (b) wait. Recommend (a).'
case_a_question_owing_a_page_is_nudged_then_stamped() {
    seed idle; W "$GENUINE"
    local before out; before="$(row_all)"
    out="$(IT "SITREP-QUESTION: $IRREV_QUESTION")"
    [ -n "$out" ] && printf '%s' "$out" | jq -e '.decision=="block" and (.reason | contains("owes a decision page") and contains("/decision-page"))' >/dev/null \
        || { echo "    no page nudge: '$out'"; return 1; }
    [ "$(row_all)" = "$before" ] || { echo "    the nudged question changed the row"; return 1; }
    out="$(IT "SITREP-QUESTION: $IRREV_QUESTION" true)"
    [ -z "$out" ] || { echo "    the continuation blocked: '$out'"; return 1; }
    expect blocked/-/q/-/- stamped && [ "$(summ)" = "$IRREV_QUESTION" ] || { echo "    summary '$(summ)'"; return 1; }
}
# An explicit blocked stamp that owes a page is refused, rc 2, naming why and the fix; with its page it stamps.
case_a_blocked_stamp_owing_a_page_is_refused() {
    seed idle
    local q='The hub is on rc.3. The laptop waits. Cut rc.4? Options: (a) yes; (b) no. Recommend (a).'
    local before err rc=0; before="$(row_all)"
    err="$("$ST" blocked "$q" 2>&1 >/dev/null)" || rc=$?
    [ "$rc" = 2 ] || { echo "    rc $rc (want 2): '$err'"; return 1; }
    case "$err" in *"blocked refused:"*"/decision-page"*) ;; *) echo "    stderr: '$err'"; return 1 ;; esac
    case "$err" in *"needs its text"*) echo "    the refusal was named as missing text: '$err'"; return 1 ;; esac
    [ "$(row_all)" = "$before" ] || { echo "    the refused stamp changed the row"; return 1; }
    "$ST" blocked "$q Page: /project/decision.html" >/dev/null || { echo "    the stamp with its page failed"; return 1; }
    jq -e --arg n "$NAME" --arg q "$q Page: /project/decision.html" '.agents[$n].question == $q' "$REGISTRY" >/dev/null \
        || { echo "    the paged question was not kept: '$(row_all)'"; return 1; }
}
case_mail_outranks_the_question_shape_nudge() {
    hold_bare_question || return 1
    seed idle; W "$GENUINE"; _mail_reset; _mail_line "$NAME"
    LT_NEW 'please choose a port'; LT_REPLY 'SITREP-QUESTION: which port?'
    local before out; before="$(row_all)"
    out="$(LT_STOP)"
    [ "$out" = "$MAIL_BLOCK" ] || { echo "    mail did not block first: '$out'"; return 1; }
    [ "$(row_all)" = "$before" ] || { echo "    the mail-blocked marker changed the row"; return 1; }
    poll_mail || return 1
}
case_marker_waiting_stamps_purple() {
    seed idle; W "$GENUINE"
    IT $'SITREP-WAITING:\n\nTwo FE peers, their launch argv; 15 min fallback.' >/dev/null
    expect waiting/-/-/w/- state && [ "$(summ)" = "Two FE peers, their launch argv; 15 min fallback." ] || { echo "    summary '$(summ)'"; return 1; }
}
case_marker_in_continuation_still_stamps() {
    seed idle; W "$GENUINE"; "$ST" blocked "q?" >/dev/null
    local out; out="$(IT "SITREP-QUESTION: $PORT_QUESTION" true)"
    [ -z "$out" ] || { echo "    unexpected output '$out'"; return 1; }
    expect blocked/-/q/-/- state && [ "$(summ)" = "$PORT_QUESTION" ]
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
    question_shape_block "$out" || return 1
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
