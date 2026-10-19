#!/usr/bin/env bash
# test-identity-row-rule.sh — a session outside any row never takes, and never
# reads the mail of, a handle a live row holds (IDENTITY-FC).
#
# Reading: a handle a row holds is read only by a process carrying that row's
# id; anything else in the folder gets "not this row's inbox" from comm-poll.sh
# and its cursor is left byte-identical.
#   a   stray session: no SOT_WORKSPACE_ID, the row's self-file reachable
#   a2  the stray's Stop hook does not block its turn end
#   b   process of ANOTHER row inheriting the self-file
#   b2  process carrying the row's own id: reads and advances
#   c   no-row shell through the shared nopane slot naming the handle
#   ctl handle with no row (empty workspace_id): polls with no row id
#   g   an unreadable registry reads nothing and moves no cursor
#   r   comm-context.sh --rebind from a process with no row id rebinds nothing
# Taking: a join from outside any row asks the daemon (agent.join with an
# empty workspace_id), which knows its live rows.
#   d   a plain join steps past a handle the daemon says a live row holds
#   d2  --name of one is refused with the daemon's message, nothing written
#   d3  --name of one the daemon says no live row holds (the row is gone) is taken
#   d4  --name of a row-bound handle with no daemon to ask is refused
#   e   a respawned row's join takes its handle back
#
# HERMETIC, same seams as test-agent-join.sh (a stub daemon on a unix socket)
# and test-comm-poll-cursor.sh. Never the live comm home or daemon.
#
# Usage: comm/tests/test-identity-row-rule.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home
. "$(dirname "${BASH_SOURCE[0]}")/lib-wait.sh" || exit 2

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-identity-row-rule-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
export SOTD_BIN="$(guard_bridge_stub "$WORK/bridge")"
[ -x "$SOTD_BIN" ] || exit 2
mkdir -p "$SOT_COMM_HOME"
JOIN="$SCRIPTS_DIR/comm-join.sh"
POLL="$SCRIPTS_DIR/comm-poll.sh"
CONTEXT="$SCRIPTS_DIR/comm-context.sh"
IDLE_HOOK="$SCRIPTS_DIR/comm-status-idle.sh"

STUB_NC_PID=""; STUB_WATCHER_PID=""
cleanup() {
    end_group "$STUB_WATCHER_PID"
    end_group "$STUB_NC_PID"
    exec 3>&- 2>/dev/null || true
    rm -rf "${WORK:?}"
}
trap cleanup EXIT

HOST="testhost"
export SOT_COMM_TEST_HOST="$HOST"
NAME="t-ident"
ROW="row-w"
SELF="$WORK/self.txt"
NOPANE="$WORK/${HOST}__nopane.txt"
REG="$SOT_COMM_HOME/registry.json"
INBOX="$SOT_COMM_HOME/inbox/$NAME.jsonl"
CURSOR="$SOT_COMM_HOME/read/$NAME.cursor"

( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF" "$JOIN" --name "$NAME" ) >/dev/null 2>&1 \
    || { echo "FATAL: setup join failed" >&2; exit 1; }
cp "$SELF" "$NOPANE"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"   # the Stop hook finds comm-status.sh here

# --- the stub daemon: answers hello, and agent.join from outside any row ----
# (an empty workspace_id) with held_by_row for each handle in $WORK/held.txt,
# else ok. Logs every request.
SOCK=""; REQLOG=""
start_stub_daemon() {
    SOCK="$WORK/stub.sock"; REQLOG="$WORK/req.log"
    local fifo="$WORK/resp.fifo"
    rm -f "$SOCK" "$fifo"; mkfifo "$fifo"; : > "$REQLOG"; : >> "$WORK/held.txt"
    exec 3<>"$fifo"
    set -m   # each step leads a process group of its own, which end_group (lib-wait.sh) ends whole
    nc -klU "$SOCK" < "$fifo" >> "$REQLOG" &
    STUB_NC_PID=$!
    ( tail -n +1 -F "$REQLOG" 2>/dev/null | while IFS= read -r line; do
        case "$(printf '%s' "$line" | jq -r '.op // empty' 2>/dev/null)" in
            hello) printf '%s\n' '{"v":1,"id":1,"kind":"res","op":"hello","payload":{"session_id":"s1","revision":0,"snapshot_pending":false}}' >&3 ;;
            agent.join)
                h="$(printf '%s' "$line" | jq -r '.payload.handle')"
                if grep -qxF "$h" "$WORK/held.txt"; then
                    jq -nc --arg h "$h" '{v:1,id:1,kind:"res",op:"agent.join",payload:{error:("@" + $h + " is held by a live row; a session outside any row cannot join as it"),code:"held_by_row"}}' >&3
                else
                    printf '%s\n' '{"v":1,"id":1,"kind":"res","op":"agent.join","payload":{"ok":true}}' >&3
                fi ;;
        esac
    done ) &
    STUB_WATCHER_PID=$!
    set +m
    await test -S "$SOCK" || echo "stub daemon socket never appeared: $SOCK" >&2
}
stop_stub_daemon() {
    end_group "$STUB_WATCHER_PID"; end_group "$STUB_NC_PID"
    exec 3>&- 2>/dev/null || true
    STUB_NC_PID=""; STUB_WATCHER_PID=""
}

PASS=0; FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}

bind_row() {  # WORKSPACE_ID ("" = no row)
    jq --arg n "$NAME" --arg w "$1" '.agents[$n].workspace_id = $w' "$REG" > "$REG.new" \
        && mv "$REG.new" "$REG"
}
reset_inbox() {
    mkdir -p "$(dirname "$INBOX")" "$(dirname "$CURSOR")"
    jq -nc --arg to "$NAME" '{from:"peer",to:$to,repo:"r",msg:"unread-one",ts:"2026-01-01T00:00:01Z"}' > "$INBOX"
    printf '%s' "0" > "$CURSOR"
}
poll() {  # SELF_FILE [env assignments...] -> POLL_OUT / POLL_RC
    local self="$1"; shift
    POLL_OUT="$(cd "$WORK" && env SOT_COMM_SELF_FILE="$self" "$@" "$POLL" 2>&1)"
    POLL_RC=$?
}
refused() {  # want nonzero, the refusal text, cursor untouched
    local before after
    before="$(cat "$CURSOR")"
    poll "$@"
    after="$(cat "$CURSOR")"
    [ "$POLL_RC" -ne 0 ] || { echo "  poll exited 0: $POLL_OUT"; return 1; }
    case "$POLL_OUT" in *"not this row's inbox"*) ;; *) echo "  no refusal text: $POLL_OUT"; return 1 ;; esac
    case "$POLL_OUT" in *unread-one*) echo "  the inbox was shown: $POLL_OUT"; return 1 ;; esac
    [ "$before" = "$after" ] || { echo "  cursor moved '$before' -> '$after'"; return 1; }
}

case_a_stray_session() { bind_row "$ROW"; reset_inbox; refused "$SELF"; }
case_b_other_row() { bind_row "$ROW"; reset_inbox; refused "$SELF" SOT_WORKSPACE_ID=row-other; }
case_b2_own_row_reads() {
    bind_row "$ROW"; reset_inbox
    poll "$SELF" SOT_WORKSPACE_ID="$ROW"
    [ "$POLL_RC" -eq 0 ] || { echo "  poll exited $POLL_RC: $POLL_OUT"; return 1; }
    case "$POLL_OUT" in *unread-one*) ;; *) echo "  not shown: $POLL_OUT"; return 1 ;; esac
    [ "$(cut -d" " -f1 "$CURSOR")" = "1" ] || { echo "  cursor '$(cat "$CURSOR")'"; return 1; }
}
case_c_nopane_slot() { bind_row "$ROW"; reset_inbox; refused "$NOPANE"; }
case_ctl_no_row_polls() {
    bind_row ""; reset_inbox
    poll "$SELF"
    [ "$POLL_RC" -eq 0 ] || { echo "  poll exited $POLL_RC: $POLL_OUT"; return 1; }
    case "$POLL_OUT" in *unread-one*) ;; *) echo "  not shown: $POLL_OUT"; return 1 ;; esac
}
case_g_unreadable_registry_reads_nothing() {
    bind_row "$ROW"; reset_inbox
    local before; before="$(cat "$CURSOR")"
    cp "$REG" "$WORK/registry.saved"; : > "$REG"
    poll "$NOPANE"
    cp "$WORK/registry.saved" "$REG"
    [ "$POLL_RC" -ne 0 ] || { echo "  poll exited 0: $POLL_OUT"; return 1; }
    case "$POLL_OUT" in *"the registry could not be read; nothing was read"*) ;; *) echo "  no unreadable line: $POLL_OUT"; return 1 ;; esac
    case "$POLL_OUT" in *"not this row's inbox"*) echo "  called another row's inbox: $POLL_OUT"; return 1 ;; esac
    case "$POLL_OUT" in *unread-one*) echo "  the inbox was shown: $POLL_OUT"; return 1 ;; esac
    [ "$(cat "$CURSOR")" = "$before" ] || { echo "  cursor moved '$before' -> '$(cat "$CURSOR")'"; return 1; }
}
case_r_rebind_needs_a_row_id() {
    bind_row "$ROW"
    local out
    out="$(cd "$WORK" && env SOT_COMM_SELF_FILE="$SELF" "$CONTEXT" --rebind 2>/dev/null | sed -n 's/^NAME=//p')"
    [ -z "$out" ] || { echo "  a process with no row id kept @$out through --rebind"; return 1; }
}

stop_hook() {  # [env...] -> HOOK_OUT / HOOK_RC
    HOOK_OUT="$(cd "$WORK" && echo '{}' | env SOT_COMM_SELF_FILE="$SELF" HOME="$WORK" "$@" "$IDLE_HOOK" 2>/dev/null)"
    HOOK_RC=$?
}
case_a2_stray_stop_hook_does_not_block() {
    bind_row "$ROW"; reset_inbox
    # Control: the owner's hook blocks on the unread mail, so a quiet stray is not a vacuous pass.
    stop_hook SOT_WORKSPACE_ID="$ROW"
    case "$HOOK_OUT" in *block*) ;; *) echo "  the owner's hook did not block (test is vacuous): $HOOK_OUT"; return 1 ;; esac
    stop_hook
    [ "$HOOK_RC" -eq 0 ] || { echo "  hook exited $HOOK_RC"; return 1; }
    case "$HOOK_OUT" in *block*) echo "  stray's hook blocked: $HOOK_OUT"; return 1 ;; esac
}
case_e_respawned_row_rejoins() {
    bind_row "row-w1"; reset_inbox
    cp "$SELF" "$WORK/self.save"; cp "$REG" "$WORK/reg.save"
    ( cd "$WORK" && env SOT_COMM_SELF_FILE="$SELF" SOT_WORKSPACE_ID=row-w2 "$JOIN" ) > "$WORK/join.out" 2>&1 \
        || { echo "  join failed: $(cat "$WORK/join.out")"; return 1; }
    local h
    h="$(cd "$WORK" && env SOT_COMM_SELF_FILE="$SELF" SOT_WORKSPACE_ID=row-w2 "$CONTEXT" 2>/dev/null | sed -n 's/^NAME=//p')"
    [ "$h" = "$NAME" ] || { echo "  join gave '$h', want $NAME: $(cat "$WORK/join.out")"; return 1; }
    [ "$(jq -r --arg n "$NAME" '.agents[$n].workspace_id' "$REG")" = "row-w2" ] || { echo "  registry not rebound"; return 1; }
    poll "$SELF" SOT_WORKSPACE_ID=row-w2
    local ok=0
    [ "$POLL_RC" -eq 0 ] && case "$POLL_OUT" in *unread-one*) ok=1 ;; esac
    cp "$WORK/self.save" "$SELF"; cp "$WORK/reg.save" "$REG"   # the other cases reuse the seed
    [ "$ok" = 1 ] || { echo "  poll as the new row: $POLL_OUT"; return 1; }
    [ "$(cut -d" " -f1 "$CURSOR")" = "1" ] || { echo "  cursor '$(cat "$CURSOR")'"; return 1; }
}

# --- taking: a stray (no row id, no self-file) in the row's folder ------------
STRAY_N=0
stray_join() {  # [join args...] -> JOIN_OUT / JOIN_RC ; fresh self-file each time; SOT_SOCKET from the caller
    STRAY_N=$((STRAY_N + 1)); STRAY_SELF="$WORK/stray-$STRAY_N.txt"
    JOIN_OUT="$(cd "$WORK" && env SOT_COMM_SELF_FILE="$STRAY_SELF" "$JOIN" "$@" 2>&1)"
    JOIN_RC=$?
}
seed_derived_bound() {  # sets DH: the handle a plain stray join derives, now bound to $ROW with mail and a cursor
    SOT_SOCKET="" stray_join; [ "$JOIN_RC" -eq 0 ] || { echo "  seed join failed: $JOIN_OUT"; return 1; }
    DH="$(sed -n 1p "$STRAY_SELF")"
    jq --arg n "$DH" --arg w "$ROW" '.agents[$n].workspace_id = $w' "$REG" > "$REG.new" && mv "$REG.new" "$REG"
    mkdir -p "$SOT_COMM_HOME/inbox" "$SOT_COMM_HOME/read"
    jq -nc --arg to "$DH" '{from:"peer",to:$to,repo:"r",msg:"unread-one",ts:"2026-01-01T00:00:01Z"}' > "$SOT_COMM_HOME/inbox/$DH.jsonl"
    printf '%s' "0" > "$SOT_COMM_HOME/read/$DH.cursor"
}
held_by() { printf '%s\n' "$@" > "$WORK/held.txt"; }

case_d_stray_join_steps_past_a_held_handle() {
    seed_derived_bound || return 1
    held_by "$DH"; start_stub_daemon
    local reg_before cur_before
    reg_before="$(jq -c --arg n "$DH" '.agents[$n]' "$REG")"; cur_before="$(cat "$SOT_COMM_HOME/read/$DH.cursor")"
    SOT_SOCKET="$SOCK" stray_join
    stop_stub_daemon
    [ "$JOIN_RC" -eq 0 ] || { echo "  join failed: $JOIN_OUT"; return 1; }
    [ "$(sed -n 1p "$STRAY_SELF")" != "$DH" ] || { echo "  stray took @$DH"; return 1; }
    [ "$(jq -c --arg n "$DH" '.agents[$n]' "$REG")" = "$reg_before" ] || { echo "  registry entry for @$DH changed"; return 1; }
    [ "$(cat "$SOT_COMM_HOME/read/$DH.cursor")" = "$cur_before" ] || { echo "  cursor changed"; return 1; }
}
case_d2_stray_name_of_a_held_handle_is_refused() {
    seed_derived_bound || return 1
    held_by "$DH"; start_stub_daemon
    local reg_before cur_before
    reg_before="$(cat "$REG")"; cur_before="$(cat "$SOT_COMM_HOME/read/$DH.cursor")"
    SOT_SOCKET="$SOCK" stray_join --name "$DH"
    stop_stub_daemon
    [ "$JOIN_RC" -eq 1 ] || { echo "  exit $JOIN_RC: $JOIN_OUT"; return 1; }
    case "$JOIN_OUT" in *"@$DH is held by a live row"*"a session outside any row cannot join as it"*) ;; *) echo "  message: $JOIN_OUT"; return 1 ;; esac
    [ "$(cat "$REG")" = "$reg_before" ] || { echo "  registry changed"; return 1; }
    [ "$(cat "$SOT_COMM_HOME/read/$DH.cursor")" = "$cur_before" ] || { echo "  cursor changed"; return 1; }
}
case_d3_stray_takes_a_handle_whose_row_is_gone() {
    seed_derived_bound || return 1
    held_by ""; start_stub_daemon
    SOT_SOCKET="$SOCK" stray_join --name "$DH"
    stop_stub_daemon
    [ "$JOIN_RC" -eq 0 ] || { echo "  join failed: $JOIN_OUT"; return 1; }
    [ "$(jq -r --arg n "$DH" '.agents[$n].workspace_id' "$REG")" = "" ] || { echo "  @$DH still bound to a row"; return 1; }
}
case_d4_stray_name_of_a_bound_handle_with_no_daemon_is_refused() {
    seed_derived_bound || return 1
    local reg_before; reg_before="$(cat "$REG")"
    SOT_SOCKET="$WORK/no-daemon.sock" stray_join --name "$DH"
    [ "$JOIN_RC" -eq 1 ] || { echo "  exit $JOIN_RC: $JOIN_OUT"; return 1; }
    case "$JOIN_OUT" in *"@$DH is bound to row $ROW"*"a session outside any row cannot join as it"*) ;; *) echo "  message: $JOIN_OUT"; return 1 ;; esac
    [ "$(cat "$REG")" = "$reg_before" ] || { echo "  registry changed"; return 1; }
}

check "a: a stray session in the folder is refused, cursor untouched" case_a_stray_session
check "a2: the stray's Stop hook does not block its turn end" case_a2_stray_stop_hook_does_not_block
check "e: a respawned row rejoins and polls as the new row" case_e_respawned_row_rejoins
check "b: a process of another row is refused, cursor untouched" case_b_other_row
check "b2: a process carrying the row's id reads and advances" case_b2_own_row_reads
check "c: the nopane slot naming a row's handle is refused" case_c_nopane_slot
check "ctl: a handle with no row polls with no row id" case_ctl_no_row_polls
check "g: an unreadable registry reads nothing and moves no cursor" case_g_unreadable_registry_reads_nothing
check "r: --rebind from a process with no row id rebinds nothing" case_r_rebind_needs_a_row_id
check "d: a stray's plain join steps past a handle the daemon says a live row holds" case_d_stray_join_steps_past_a_held_handle
check "d2: a stray's --name of a held handle is refused with the daemon's message" case_d2_stray_name_of_a_held_handle_is_refused
check "d3: a stray takes a bound handle whose row the daemon says is gone" case_d3_stray_takes_a_handle_whose_row_is_gone
check "d4: a stray's --name of a bound handle is refused when no daemon can be asked" case_d4_stray_name_of_a_bound_handle_with_no_daemon_is_refused

echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
