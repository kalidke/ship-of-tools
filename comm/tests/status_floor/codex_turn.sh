# Run the actual installed command with the observed envelope fields.
codex_hook_home="$WORK/codex-hook-home"
mkdir "$codex_hook_home" && ln -s "$SOT_COMM_HOME" "$codex_hook_home/.sot-comm" || exit 1
codex_hook() {
    local ev=$1 session=$2 turn=$3 extra=${4:-'{}'} command input
    command=$(jq -r --arg ev "$ev" '.hooks[$ev][0].hooks[0].command // empty' "$HOOKS_DIR/../../adapters/codex/hooks.json") || return 1
    [ -n "$command" ] || return 0  # no hook registered for this event
    input=$(jq -nc --arg e "$ev" --arg s "$session" --arg t "$turn" --argjson extra "$extra" \
        '{hook_event_name:$e, prompt:"go", tool_name:"Bash"} + (if $s=="" then {} else {session_id:$s} end)
        + (if $t=="" then {} else {turn_id:$t} end) + $extra') || return 1
    HOME="$codex_hook_home" SOT_AGENT_KIND=codex SOT_WORKSPACE_ID=codex-turn bash -c "$command" <<< "$input"
}
codex_facts() {
    jq -e --arg h "$NAME" ".agents[\$h] | $1" "$REGISTRY" >/dev/null && return 0
    echo "    expected $1 in $(jq -c --arg h "$NAME" '.agents[$h]' "$REGISTRY")" >&2
    return 1
}
codex_marker="$SOT_COMM_HOME/state/dialog-codex-turn.marker"

early=01990000-0001-7000-8000-000000000001
late=01990000-0002-7000-8000-000000000001

case_codex_interrupt() (
    seed idle
    codex_hook UserPromptSubmit a one && "$ST" waiting build && codex_hook PermissionRequest a one || exit 1
    sot_comm stop-mark "$NAME" && cp "$REGISTRY" "$WORK/before-interrupt" || exit 1
    for ids in 'b one' 'a two'; do
        codex_hook Interrupt $ids && cmp "$WORK/before-interrupt" "$REGISTRY" || exit 1
    done
    codex_hook Interrupt a "" && codex_hook Interrupt "" one && codex_hook Interrupt a one '{"agent_id":"child"}' &&
        cmp "$WORK/before-interrupt" "$REGISTRY" && [ -f "$codex_marker" ] || exit 1
    codex_hook Interrupt a one && [ -f "$codex_marker" ] &&
        codex_facts '.state=="waiting" and .waiting=="build" and ([.dialog,.floor,.stop_at,.codex_turn] | all(.==null))' || exit 1
    seed_facts '{"floor":"user","dialog":"pending"}'
    cp "$REGISTRY" "$WORK/no-owner" && codex_hook Interrupt a one && cmp "$WORK/no-owner" "$REGISTRY"
)

case_codex_late_stop() (
    seed idle; _mail_reset
    codex_hook UserPromptSubmit a "$early" && codex_hook UserPromptSubmit a "$late" && codex_hook PermissionRequest a "$late" || exit 1
    cp "$REGISTRY" "$WORK/before-stop" || exit 1
    [ -z "$(codex_hook Stop a "$early" '{"last_assistant_message":"SITREP: late result"}')" ] &&
        cmp "$WORK/before-stop" "$REGISTRY" && _mail_line "$NAME" || exit 1
    [ -z "$(codex_hook Stop a "$early")" ] && cmp "$WORK/before-stop" "$REGISTRY" && [ -f "$codex_marker" ] || exit 1
    [ -z "$(codex_hook Stop b "$early")" ] && cmp "$WORK/before-stop" "$REGISTRY" &&
        [ -n "$(codex_hook Stop a "$late")" ]
)

case_codex_late_tool() (
    seed idle
    codex_hook UserPromptSubmit a "$early" && codex_hook UserPromptSubmit a "$late" && codex_hook PermissionRequest a "$late" || exit 1
    cp "$REGISTRY" "$WORK/before-tool" || exit 1
    codex_hook PostToolUse a "$early" && cmp "$WORK/before-tool" "$REGISTRY" && [ -f "$codex_marker" ] &&
        codex_hook PermissionRequest b "$early" '{"tool_name":"late"}' && cmp "$WORK/before-tool" "$REGISTRY" &&
        codex_hook PostToolUse a "$late" && codex_facts '.dialog==null' && [ ! -e "$codex_marker" ]
)

case_codex_held_stop() (
    seed idle; _mail_reset; _mail_line "$NAME"
    codex_hook UserPromptSubmit a one && codex_hook Stop a one | jq -e '.decision=="block" and (.reason|contains("comm-poll"))' >/dev/null &&
        codex_facts '.codex_turn==["a","one"] and .floor=="user" and .stop_at!=null' &&
        printf 1 > "$SOT_COMM_HOME/read/$NAME.cursor" &&
        [ -z "$(codex_hook Stop a one '{"stop_hook_active":true}')" ] &&
        codex_facts '.state=="done" and .codex_turn==null and .floor==null and .stop_at==null'
)

case_codex_child_events() (
    local child='{"agent_id":"child"}'
    seed idle; _mail_reset
    codex_hook UserPromptSubmit a one && codex_hook UserPromptSubmit a two "$child" &&
        codex_facts '.codex_turn==["a","one"]' && codex_hook PermissionRequest a two "$child" &&
        codex_facts '.dialog=="codex permission request: Bash" and .codex_turn==["a","one"]' && [ -f "$codex_marker" ] &&
        codex_hook PostToolUse a two "$child" && codex_facts '.dialog==null and .codex_turn==["a","one"]' &&
        [ ! -e "$codex_marker" ] && codex_hook Stop a one && codex_facts '.state=="done" and .codex_turn==null'
)

case_codex_fallbacks() (
    local variant previous turn mode event_turn
    _mail_reset
    for variant in no-owner no-turn partial-owner lost-prompt old-sotd; do (
        previous='{"floor":"user"}'; turn=two
        case "$variant" in
            no-turn) previous='{"floor":"user","codex_turn":["a","one"]}'; turn="" ;;
            lost-prompt) previous='{"floor":"user","codex_turn":["a","one"]}' ;;
            partial-owner) previous='{"floor":"user","codex_turn":["a",""]}' ;;
            old-sotd)
                # An older sotd ignores hook environment additions and lacks interrupt.
                cat > "$WORK/old-sotd" <<'OLD'
#!/usr/bin/env bash
unset SOT_CODEX_TURN SOT_CODEX_PROMPT
[ "$1 $2" != "work-state interrupt" ] || exit 2
exec sotd "$@"
OLD
                chmod +x "$WORK/old-sotd" && export SOTD_BIN="$WORK/old-sotd" || exit 1 ;;
        esac
        seed_facts "$previous"
        case "$variant" in
            lost-prompt)
                codex_hook Stop a one '{"stop_hook_active":true}' && codex_facts '.codex_turn==null' &&
                    cp "$REGISTRY" "$WORK/finished" || exit 1
                printf 'elsewhere:-:-:-:1:-\n' > "$SOT_COMM_HOME/.registry.lock" || exit 1
                SOT_LOCK_WAIT_SECS=0 codex_hook UserPromptSubmit a two || exit 1
                rm -f "${SOT_COMM_HOME:?}/.registry.lock" && cmp "$WORK/finished" "$REGISTRY" || exit 1 ;;
            old-sotd)
                codex_hook UserPromptSubmit a two && codex_facts '.floor=="user" and .codex_turn==null' || exit 1 ;;
        esac
        cp "$REGISTRY" "$WORK/fallback-start" || exit 1
        # The same input without a turn id runs the original, unowned path. Compare
        # every registry field except wall-clock stamps, and the dialog marker.
        for mode in main actual; do
            cp "$WORK/fallback-start" "$REGISTRY" || exit 1
            event_turn=$turn; [ "$mode" != main ] || event_turn=""
            codex_hook PermissionRequest a "$event_turn" && [ -f "$codex_marker" ] || exit 1
            jq -S '.agents[] | del(.status_at,.last_seen)' "$REGISTRY" > "$WORK/$mode-dialog"
            if [ "$variant" = old-sotd ]; then
                codex_hook PostToolUse a "$event_turn" && [ ! -e "$codex_marker" ] &&
                    codex_facts '.dialog==null and .floor=="user"' && codex_hook PermissionRequest a "$event_turn" || exit 1
                codex_hook Interrupt a two && [ -f "$codex_marker" ] || exit 1
            fi
            codex_hook Stop a "$event_turn" '{"stop_hook_active":true}' && [ ! -e "$codex_marker" ] || exit 1
            jq -S '.agents[] | del(.status_at,.last_seen)' "$REGISTRY" > "$WORK/$mode-stop"
        done
        cmp "$WORK/main-dialog" "$WORK/actual-dialog" && cmp "$WORK/main-stop" "$WORK/actual-stop"
    ) || { echo "    fallback failed for $variant"; exit 1; }; done
)

case_codex_uncertain_stop_checks_mail() (
    local fault
    for fault in unreadable locked; do (
        seed idle; _mail_reset; _mail_line "$NAME"
        codex_hook UserPromptSubmit a two || exit 1
        case "$fault" in
            unreadable) printf 'unreadable' > "$REGISTRY" ;;
            locked) printf 'elsewhere:-:-:-:1:-\n' > "$SOT_COMM_HOME/.registry.lock" ;;
        esac
        codex_hook Stop a one | jq -e '.decision=="block" and (.reason|contains("comm-poll"))' >/dev/null
    ) || { echo "    $fault registry prevented the mail check"; exit 1; }; done
    rm -f "${SOT_COMM_HOME:?}/.registry.lock"
)

case_codex_lost_claim_after_unclosed_turn() (
    seed idle; _mail_reset
    codex_hook UserPromptSubmit a "$early" || exit 1
    printf 'elsewhere:-:-:-:1:-\n' > "$SOT_COMM_HOME/.registry.lock" || exit 1
    SOT_LOCK_WAIT_SECS=0 codex_hook UserPromptSubmit b "$late"
    rm -f "${SOT_COMM_HOME:?}/.registry.lock"
    codex_facts ".codex_turn==[\"a\",\"$early\"]" || exit 1
    # Make the heartbeat eligible and observe its tick before the dialog writes a fresh stamp.
    jq --arg h "$NAME" '.agents[$h].status_at="2000-01-01T00:00:00Z"' "$REGISTRY" > "$WORK/stale" && mv "$WORK/stale" "$REGISTRY" || exit 1
    rm -f "$SOT_COMM_HOME/state/hb-codex-turn.tick" "$codex_marker"
    codex_hook PostToolUse b "$late" && codex_facts '.status_at!="2000-01-01T00:00:00Z"' &&
        codex_hook PermissionRequest b "$late" && codex_facts '.dialog=="codex permission request: Bash"' &&
        codex_hook PostToolUse b "$late" && codex_facts '.dialog==null' && [ ! -e "$codex_marker" ] &&
        _mail_line "$NAME" && codex_hook Stop b "$late" | jq -e '.decision=="block" and (.reason|contains("comm-poll"))' >/dev/null &&
        codex_facts ".stop_at!=null and .codex_turn==[\"a\",\"$early\"]" &&
        codex_hook Stop b "$late" '{"stop_hook_active":true}' && codex_facts ".floor==null and .codex_turn==[\"a\",\"$early\"]"
)

case_codex_unkeyed_root() (
    seed idle
    codex_hook UserPromptSubmit a "$early" && codex_hook UserPromptSubmit b "" &&
        codex_hook PermissionRequest b "" && codex_hook Interrupt a "$early" &&
        codex_facts '.codex_turn==null and .floor=="user" and .dialog=="codex permission request: Bash"' && [ -f "$codex_marker" ]
)

case_codex_interrupt_keeps_marker() (
    seed idle
    codex_hook UserPromptSubmit a "$early" && codex_hook PermissionRequest a "$early" &&
        SOT_WORKSPACE_ID=codex-turn SOT_CODEX_TURN="[\"a\",\"$early\"]" "$ST" interrupt && [ -f "$codex_marker" ] &&
        codex_hook UserPromptSubmit b "$late" && [ ! -e "$codex_marker" ]
)

case_codex_missing_helper() (
    local event err="$WORK/missing-helper.err" helper="$SCRIPTS_DIR/codex-turn.sh"
    seed idle; _mail_reset
    mv "$helper" "$WORK/held-helper" || exit 1
    trap 'mv "$WORK/held-helper" "$helper"' EXIT
    for event in UserPromptSubmit PermissionRequest PostToolUse Interrupt Stop; do
        codex_hook "$event" a "$early" '{"stop_hook_active":true}' > "$WORK/missing-helper.out" 2>"$err" &&
            [ ! -s "$err" ] && [ ! -s "$WORK/missing-helper.out" ] || { cat "$err"; exit 1; }
        case $event in
            UserPromptSubmit) codex_facts '.floor=="user" and .codex_turn==null' || exit 1 ;;
            PermissionRequest) codex_facts '.dialog=="codex permission request: Bash"' || exit 1 ;;
            PostToolUse|Interrupt) codex_facts '.floor=="user" and .dialog==null' || exit 1 ;;
            Stop) codex_facts '.floor==null' || exit 1 ;;
        esac
    done
)

case_codex_interrupt_lock_budget() (
    local holder elapsed lock="$SOT_COMM_HOME/.registry.lock"
    seed idle; codex_hook UserPromptSubmit a "$early" || exit 1
    bash -c 'printf "fixture:-:-:-:%s:-\n" "$$" > "$1" && exec sleep 30' _ "$lock" &
    holder=$!
    trap 'kill "$holder" 2>/dev/null; wait "$holder" 2>/dev/null; rm -f "$lock"' EXIT
    await test -s "$lock" || exit 1
    TIMEFORMAT='%R'; export LC_ALL=C
    elapsed=$( { time SOT_LOCK_WAIT_SECS=12 codex_hook Interrupt a "$early"; } 2>&1 ) || exit 1
    kill -0 "$holder" && [ -s "$lock" ] && codex_facts '.floor=="user"' &&
        awk -v secs="$elapsed" 'BEGIN {print "    interrupt elapsed " secs " s"; exit !(secs ~ /^[0-9]+[.][0-9]+$/ && secs+0 < 5)}'
)

case_codex_binary_input() (
    local hook
    seed idle
    for hook in comm-status-working.sh codex-status-blocked.sh; do
        printf '{"padding":"a\000b","session_id":"s","turn_id":"t","prompt":"[relay] from peer","tool_name":"Bash"}' |
            SOT_AGENT_KIND=codex bash "$SCRIPTS_DIR/$hook" 2>"$WORK/nul.err" || exit 1
        [ ! -s "$WORK/nul.err" ] || { cat "$WORK/nul.err"; exit 1; }
    done
    codex_facts '.floor=="machine" and .dialog=="codex permission request: Bash"'
)
