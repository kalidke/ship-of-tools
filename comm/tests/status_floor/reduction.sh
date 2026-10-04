# Part of ../test-status-floor.sh: the reduction cases and the lifecycle built on it.
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
    # Summary is NOT the question here: mid-turn state is `working` (floor
    # outranks it), and blocked no longer aliases .note to the question text
    # (fix 2 — the alias was the reason a dead question used to survive as
    # the summary long after it was answered). With no note set, summary is
    # blank until the row actually reduces to `blocked` below, where the
    # question comes from .question directly, not from .note.
    expect working/user/q/-/- mid-turn && [ "$(summ)" = "" ] || return 1
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
# A declaration that carries text supersedes the older note: a woken or
# answered row never falls back to a line written before it.
case_declared_text_supersedes_an_old_note() {
    local v want
    for v in blocked waiting; do
        seed idle; "$ST" idle "old" >/dev/null; "$ST" "$v" "new" >/dev/null
        [ "$v" = blocked ] && want=working/machine/q/-/- || want=working/machine/-/w/-
        W "$RELAY"; expect "$want" "$v, then a machine prompt" || return 1
        [ "$(summ)" = "" ] || { echo "    $v, then a machine prompt: summary '$(summ)'"; return 1; }
        W "$GENUINE"
        [ "$(summ)" = "" ] || { echo "    $v, then a user prompt: summary '$(summ)'"; return 1; }
    done
}
# A declaration without text keeps the fact's own text (the AskUserQuestion
# hook stamps blocked bare right after the model's `blocked "Q"`).
case_a_blank_declaration_keeps_its_own_text() {
    local v
    for v in blocked waiting; do
        seed idle; "$ST" idle "old" >/dev/null; "$ST" "$v" "Q" >/dev/null; "$ST" "$v" >/dev/null
        [ "$(summ)" = "Q" ] || { echo "    $v, then $v bare: summary '$(summ)'"; return 1; }
        # No text of its own to keep: refused, and nothing is written.
        seed idle; "$ST" idle "n" >/dev/null; local rc=0; "$ST" "$v" >/dev/null 2>&1 || rc=$?
        [ "$rc" = 2 ] && [ "$(summ)" = "n" ] || { echo "    $v bare over a note: rc $rc, summary '$(summ)'"; return 1; }
    done
}
# `waiting` is the model's word that nothing needs the user: it clears an open
# question, silently.
case_waiting_clears_an_open_question() {
    seed_facts '{}'; "$ST" blocked "q?" >/dev/null
    local err; err="$("$ST" waiting "the job" 2>&1 >/dev/null)"; local rc=$?
    [ "$rc" = 0 ] || { echo "    waiting rc $rc"; return 1; }
    expect waiting/-/-/w/- cleared || return 1
    [ -z "$err" ] || { echo "    stderr '$err'"; return 1; }
    [ "$(summ)" = "the job" ] || { echo "    summary '$(summ)'"; return 1; }
}
# `waiting` prints nothing, whether or not a question was pending.
case_waiting_without_a_question_is_silent() {
    local seed rc err
    for seed in '{}' '{"waiting":"old"}'; do
        seed_facts "$seed"; rc=0
        err="$("$ST" waiting "x" 2>&1 >/dev/null)" || rc=$?
        [ "$rc" = 0 ] || { echo "    $seed: rc $rc"; return 1; }
        [ -z "$err" ] || { echo "    $seed: stderr '$err'"; return 1; }
    done
}
# Under a CRLF-emitting jq (a native jq.exe on Windows) `waiting` still prints
# nothing, and a bare `waiting` over a row with no text is still refused.
case_waiting_under_a_crlf_jq() {
    local real_jq stub rc=0 err
    real_jq="$(command -v jq)" || { echo "    jq not found"; return 1; }
    stub="$WORK/crlf-stubbin"; mkdir -p "$stub"
    cat > "$stub/jq" <<STUB
#!/usr/bin/env bash
"$real_jq" "\$@" | sed \$'s/\$/\r/'
exit "\${PIPESTATUS[0]}"
STUB
    chmod +x "$stub/jq"
    seed_facts '{"question":"q?"}'
    err="$(PATH="$stub:$PATH" "$ST" waiting "x" 2>&1 >/dev/null)" || rc=$?
    [ "$rc" = 0 ] || { echo "    waiting rc $rc"; return 1; }
    [ -z "$err" ] || { echo "    stderr '$err'"; return 1; }
    expect waiting/-/-/w/- crlf || return 1
    seed_facts '{}'; rc=0
    PATH="$stub:$PATH" "$ST" waiting >/dev/null 2>&1 || rc=$?
    [ "$rc" = 2 ] || { echo "    bare waiting rc $rc"; return 1; }
    expect /-/-/-/- crlf-refused
}
# Text made only of newlines is no text: a bare declaration over it is refused.
case_newline_only_text_is_refused() {
    local v fact rc
    for v in blocked waiting; do
        fact=question; [ "$v" = waiting ] && fact=waiting
        seed_facts "$(jq -cn --arg f "$fact" '{($f): "\n\n"}')"; rc=0
        "$ST" "$v" >/dev/null 2>"$WORK/err" || rc=$?
        [ "$rc" = 2 ] || { echo "    $v: rc $rc"; return 1; }
        grep -qx "comm-status.sh: $v needs its text -- stamp discarded" "$WORK/err" \
            || { echo "    $v: stderr '$(cat "$WORK/err")'"; return 1; }
        jq -e --arg f "$fact" '.agents | to_entries[0].value[$f] == "\n\n"' "$REGISTRY" >/dev/null \
            || { echo "    $v: row changed"; return 1; }
    done
}
# A blocked or waiting with no text and no text of its own on the row is
# refused: a question or wait always carries readable text.
case_textless_blocked_or_waiting_is_refused() {
    local v arg rc
    for v in blocked waiting; do
        for arg in none empty; do
            seed_facts '{"note":"n"}'; rc=0
            if [ "$arg" = none ]; then "$ST" "$v" >/dev/null 2>"$WORK/err" || rc=$?
            else "$ST" "$v" "" >/dev/null 2>"$WORK/err" || rc=$?; fi
            [ "$rc" = 2 ] || { echo "    $v ($arg): rc $rc"; return 1; }
            expect /-/-/-/- "$v ($arg)" || return 1
            [ "$(summ)" = "prior" ] || { echo "    $v ($arg): summary '$(summ)'"; return 1; }
            grep -qx "comm-status.sh: $v needs its text -- stamp discarded" "$WORK/err" \
                || { echo "    $v ($arg): stderr '$(cat "$WORK/err")'"; return 1; }
        done
    done
}
case_askq_hook_carries_the_question() {
    seed_facts '{"floor":"machine"}'
    printf '%s' '{"tool_use_id":"t1","tool_input":{"questions":[{"question":"Ship it?"}]}}' \
        | bash "$HOOKS_DIR/comm-status-blocked.sh"
    expect blocked/-/q/-/- askq || return 1
    [ "$(summ)" = "Ship it?" ] || { echo "    summary '$(summ)'"; return 1; }
}
# A refused marker stamp must not stop the turn either: the floor stays.
case_marker_question_without_text_leaves_the_floor() {
    seed_facts '{"floor":"machine"}'
    IT $'SITREP-QUESTION:' >/dev/null
    expect /machine/-/-/- floor-kept
}
# Only the refusal (rc 2) skips the floor: any other stamp failure falls
# through to the turn's `stop`, as before. A stub comm-status.sh fails every
# declaration with rc 1 and hands `stop` to the real script.
case_non_refusal_stamp_failure_still_floors() {
    local stub="$WORK/stub-bin" f; mkdir "$stub"
    for f in "$SCRIPTS_DIR"/*; do [ "${f##*/}" = comm-status.sh ] || ln -s "$f" "$stub/${f##*/}"; done
    printf '#!/bin/bash\n[ "$1" = stop ] && exec "%s" "$@"\nexit 1\n' "$SCRIPTS_DIR/comm-status.sh" > "$stub/comm-status.sh"
    chmod +x "$stub/comm-status.sh"
    ln -sfn "$stub" "$SOT_COMM_HOME/bin"
    seed_facts '{"floor":"machine"}'
    IT $'SITREP: all done' >/dev/null
    ln -sfn "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
    expect idle/-/-/-/- floor-dropped
}
# The refusal writes no temp file: TMPDIR stays empty.
case_refused_stamp_leaves_no_temp_file() {
    local v td rc
    for v in blocked waiting; do
        td="$WORK/tmp-$v"; mkdir -p "$td"; seed_facts '{"note":"n"}'; rc=0
        TMPDIR="$td" "$ST" "$v" >/dev/null 2>&1 || rc=$?
        [ "$rc" = 2 ] || { echo "    $v: rc $rc"; return 1; }
        [ -z "$(ls -A "$td")" ] || { echo "    $v: left $(ls -A "$td")"; return 1; }
    done
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
# A parked question was cleared by a PostToolUse that treated ANY completed
# AskUserQuestion as this row's own answer (badge showed idle with a real
# question still open — field report, 2026-09-27). The row here is parked
# via a plain self-report `blocked`, so no PreToolUse ever ran for it and no
# marker exists; a bare HBQ (fix 1's marker check fails to find one) must
# answer nothing.
case_parked_question_survives_a_foreign_askuserquestion_answer() {
    seed idle; W "$GENUINE"; "$ST" blocked "the question?" >/dev/null; floor_now
    expect blocked/-/q/-/- parked || return 1
    HBQ                                   # a dialog this row never opened
    expect blocked/-/q/-/- after-foreign-answer && [ "$(summ)" = "the question?" ]
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

