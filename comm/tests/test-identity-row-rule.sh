#!/usr/bin/env bash
# test-identity-row-rule.sh — a session outside any row never takes, and never
# reads the mail of, a handle a row holds (IDENTITY-FC).
#
# Reading: a handle a row holds is read only by a process carrying that row's
# id; anything else in the folder gets "not this row's inbox" from comm-poll.sh
# and its cursor is left byte-identical.
#   a   stray session: no SOT_WORKSPACE_ID, the row's self-file reachable
#   a2  the stray's Stop hook does not block its turn end
#   b   process of ANOTHER row inheriting the self-file
#   b2  process carrying the row's own id (a row-keyed self-file, beneath its capsule): reads and advances
#   b3  an id exported over a private self-file the walk cannot tie to a row is no row id (N2)
#   c   no-row shell through the shared nopane slot naming the handle
#   bind  sot_row_binding: owner, other, unbound and unreadable (N1)
#   ctl handle with no row (empty workspace_id): polls with no row id
#   g   an unreadable registry reads nothing and moves no cursor
#   r   comm-context.sh --rebind from a process with no row id rebinds nothing
# Taking: a join from outside any row asks the daemon (agent.join with an
# empty workspace_id), which knows its rows.
#   d   a plain join steps past a handle the daemon says a row holds
#   d2  --name of one is refused with the daemon's message, nothing written
#   d3  --name of one the daemon says no row holds (the row is gone) is taken
#   d4  --name of a row-bound handle with no daemon to ask is refused
#   d5  comm-leave.sh --name H of a held handle is refused and the entry stays; a join of H is refused too (B2, SF1)
#   d6  an entry bound to no row is asked about too: held, so refused (B2)
#   d7  no daemon and an entry bound to no row: taken (the named limit, N7)
#   d8  a row that binds the handle between the daemon's answer and the write keeps it (SF3)
#   d9  an entry bound on another computer is held, whatever the local daemon says (SF4)
#   d10 an unreadable registry refuses a join from outside (N4)
#   d12 no daemon: comm-leave.sh --name of a bound entry is refused, so leave then join takes nothing (SF1)
#   d13 comm-leave.sh --name of an orphan the daemon says no row holds still removes it (SF1)
#   s1  a stray whose self file names a held handle starts its session as its own handle, never reading the held one (SF2)
#   d14 a registry the clearance cannot read refuses a join from outside, though the writer could read it (N4)
#   d11 a derived join whose every tier the daemon holds takes none, empty cleared list included (bash 3.2)
#   ds  a stray's session start steps past the held handle: its one plain line, identity ok (SF1)
#   e   a respawned row's join takes its handle back
#   s3  a displaced row's next session start rejoins and says it moved, never REFUSED (SF2)
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
cp "$SOT_COMM_HOME/registry.json" "$WORK/registry.seed"   # the taking cases start from it
# A row's own identity is keyed by its id, and its process runs beneath its capsule (in_row, lib-home-guard.sh).
row_self() { cp "$SELF" "$WORK/${HOST}__$1.txt"; printf '%s\n' "$WORK/${HOST}__$1.txt"; }
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
                # A row that binds the handle while the daemon answers (SF3): handles in bind-on-ask.txt.
                if grep -qxF "$h" "$WORK/bind-on-ask.txt" 2>/dev/null; then
                    jq --arg n "$h" --arg host "$HOST" '.agents[$n] = {host:$host, workspace_id:"row-late", repo:"r", root:"/x", status:"idle"}' "$REG" > "$REG.stub" \
                        && mv "$REG.stub" "$REG"
                fi
                if grep -qxF "$h" "$WORK/held.txt" || [ -e "$WORK/held-all" ]; then
                    jq -nc --arg h "$h" '{v:1,id:1,kind:"res",op:"agent.join",payload:{error:("@" + $h + " is held by a row; a session outside any row cannot join as it"),code:"held_by_row"}}' >&3
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
poll() {  # SELF_FILE [env assignments...] -> POLL_OUT / POLL_RC ; a keyed self-file runs beneath its row's capsule
    local self="$1" wrap=() rid; shift
    case "$(basename "$self")" in
        *__*.txt) rid="$(basename "$self")"; rid="${rid#*__}"; rid="${rid%.txt}"
                  [ "$rid" = nopane ] || wrap=(in_row "$rid") ;;
    esac
    POLL_OUT="$(cd "$WORK" && "${wrap[@]+"${wrap[@]}"}" env SOT_COMM_SELF_FILE="$self" "$@" "$POLL" 2>&1)"
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
case_b_other_row() { bind_row "$ROW"; reset_inbox; refused "$(row_self row-other)" SOT_WORKSPACE_ID=row-other; }
case_b3_an_exported_id_over_a_private_file_is_no_row_id() {
    bind_row "$ROW"; reset_inbox; refused "$SELF" SOT_WORKSPACE_ID="$ROW"
}
case_b2_own_row_reads() {
    bind_row "$ROW"; reset_inbox
    poll "$(row_self "$ROW")" SOT_WORKSPACE_ID="$ROW"
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

stop_hook() {  # [SELF_FILE] [env...] -> HOOK_OUT / HOOK_RC
    local self="$SELF" wrap=()
    case "${1:-}" in /*) self="$1"; shift; wrap=(in_row "$ROW") ;; esac
    HOOK_OUT="$(cd "$WORK" && echo '{}' | "${wrap[@]+"${wrap[@]}"}" env SOT_COMM_SELF_FILE="$self" HOME="$WORK" "$@" "$IDLE_HOOK" 2>/dev/null)"
    HOOK_RC=$?
}
case_a2_stray_stop_hook_does_not_block() {
    bind_row "$ROW"; reset_inbox
    # Control: the owner's hook blocks on the unread mail, so a quiet stray is not a vacuous pass.
    stop_hook "$(row_self "$ROW")" SOT_WORKSPACE_ID="$ROW"
    case "$HOOK_OUT" in *block*) ;; *) echo "  the owner's hook did not block (test is vacuous): $HOOK_OUT"; return 1 ;; esac
    stop_hook
    [ "$HOOK_RC" -eq 0 ] || { echo "  hook exited $HOOK_RC"; return 1; }
    case "$HOOK_OUT" in *block*) echo "  stray's hook blocked: $HOOK_OUT"; return 1 ;; esac
}
case_e_respawned_row_rejoins() {
    bind_row "row-w1"; reset_inbox
    cp "$SELF" "$WORK/self.save"; cp "$REG" "$WORK/reg.save"
    local self2; self2="$(row_self row-w2)"
    ( cd "$WORK" && in_row row-w2 env SOT_COMM_SELF_FILE="$self2" SOT_WORKSPACE_ID=row-w2 "$JOIN" ) > "$WORK/join.out" 2>&1 \
        || { echo "  join failed: $(cat "$WORK/join.out")"; return 1; }
    local h
    h="$(cd "$WORK" && in_row row-w2 env SOT_COMM_SELF_FILE="$self2" SOT_WORKSPACE_ID=row-w2 "$CONTEXT" 2>/dev/null | sed -n 's/^NAME=//p')"
    [ "$h" = "$NAME" ] || { echo "  join gave '$h', want $NAME: $(cat "$WORK/join.out")"; return 1; }
    [ "$(jq -r --arg n "$NAME" '.agents[$n].workspace_id' "$REG")" = "row-w2" ] || { echo "  registry not rebound"; return 1; }
    poll "$self2" SOT_WORKSPACE_ID=row-w2
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
    cp "$WORK/registry.seed" "$REG"
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
    local own; own="$(sed -n 1p "$STRAY_SELF")"
    [ "$own" != "$DH" ] || { echo "  stray took @$DH"; return 1; }
    [ "$(jq -c --arg n "$DH" '.agents[$n]' "$REG")" = "$reg_before" ] || { echo "  registry entry for @$DH changed"; return 1; }
    [ "$(cat "$SOT_COMM_HOME/read/$DH.cursor")" = "$cur_before" ] || { echo "  cursor changed"; return 1; }
    # SF1: one plain line naming the held handle, no stranding warning.
    case "$JOIN_OUT" in *"comm: @$DH is held by a row; a session outside any row joins as @$own"*) ;; *) echo "  no plain line: $JOIN_OUT"; return 1 ;; esac
    case "$JOIN_OUT" in *WARNING*|*heartbeated*|*"already held by"*) echo "  stranding text: $JOIN_OUT"; return 1 ;; esac
}
case_ds_stray_session_start_steps_past() {
    seed_derived_bound || return 1
    held_by "$DH"; start_stub_daemon
    local out
    out="$(cd "$WORK" && env SOT_COMM_SELF_FILE="$WORK/stray-ds.txt" SOT_SOCKET="$SOCK" HOME="$WORK" "$SCRIPTS_DIR/comm-session-start.sh" 2>&1)"
    stop_stub_daemon
    case "$out" in *"BOOTSTRAP-ARM handle="*"identity=ok"*) ;; *) echo "  no identity=ok: $out"; return 1 ;; esac
    case "$out" in *"handle=$DH "*) echo "  took @$DH: $out"; return 1 ;; esac
    case "$out" in *MISMATCH*|*REFUSED*) echo "  MISMATCH or REFUSED: $out"; return 1 ;; esac
}
case_d2_stray_name_of_a_held_handle_is_refused() {
    seed_derived_bound || return 1
    held_by "$DH"; start_stub_daemon
    local reg_before cur_before
    reg_before="$(cat "$REG")"; cur_before="$(cat "$SOT_COMM_HOME/read/$DH.cursor")"
    SOT_SOCKET="$SOCK" stray_join --name "$DH"
    stop_stub_daemon
    [ "$JOIN_RC" -eq 1 ] || { echo "  exit $JOIN_RC: $JOIN_OUT"; return 1; }
    case "$JOIN_OUT" in *"@$DH is held by a row"*"a session outside any row cannot join as it"*) ;; *) echo "  message: $JOIN_OUT"; return 1 ;; esac
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

case_d5_leave_then_join_is_refused() {
    seed_derived_bound || return 1
    held_by "$DH"; start_stub_daemon
    : > "$REQLOG"
    local out rc=0
    out="$(cd "$WORK" && env SOT_COMM_SELF_FILE="$WORK/stray-leave.txt" SOT_SOCKET="$SOCK" "$SCRIPTS_DIR/comm-leave.sh" --name "$DH" 2>&1)" || rc=$?
    [ "$rc" -eq 1 ] || { stop_stub_daemon; echo "  leave exit $rc: $out"; return 1; }
    case "$out" in *"REFUSING"*"@$DH is held by a row"*) ;; *) stop_stub_daemon; echo "  leave message: $out"; return 1 ;; esac
    jq -e --arg n "$DH" '.agents | has($n)' "$REG" >/dev/null || { stop_stub_daemon; echo "  leave removed @$DH"; return 1; }
    SOT_SOCKET="$SOCK" stray_join --name "$DH"
    local asked; asked="$(grep -c "\"handle\":\"$DH\"" "$REQLOG")"
    stop_stub_daemon
    [ "$JOIN_RC" -eq 1 ] || { echo "  exit $JOIN_RC: $JOIN_OUT"; return 1; }
    case "$JOIN_OUT" in *"@$DH is held by a row"*) ;; *) echo "  message: $JOIN_OUT"; return 1 ;; esac
    [ "$asked" -ge 2 ] || { echo "  the daemon was asked $asked times about @$DH, want the leave's and the join's"; return 1; }
}
case_d12_no_daemon_leave_of_a_bound_entry_is_refused() {
    seed_derived_bound || return 1
    local out rc=0 before; before="$(cat "$REG")"
    out="$(cd "$WORK" && env SOT_COMM_SELF_FILE="$WORK/stray-leave.txt" SOT_SOCKET="$WORK/no-daemon.sock" "$SCRIPTS_DIR/comm-leave.sh" --name "$DH" 2>&1)" || rc=$?
    [ "$rc" -eq 1 ] || { echo "  leave exit $rc: $out"; return 1; }
    case "$out" in *REFUSING*"@$DH is bound to row $ROW"*) ;; *) echo "  leave message: $out"; return 1 ;; esac
    [ "$(cat "$REG")" = "$before" ] || { echo "  registry changed"; return 1; }
    SOT_SOCKET="$WORK/no-daemon.sock" stray_join --name "$DH"
    [ "$JOIN_RC" -eq 1 ] || { echo "  leave then join: exit $JOIN_RC: $JOIN_OUT"; return 1; }
}
case_d13_leave_of_an_orphan_the_daemon_does_not_hold_removes_it() {
    seed_derived_bound || return 1
    held_by ""; start_stub_daemon
    local out rc=0
    out="$(cd "$WORK" && env SOT_COMM_SELF_FILE="$WORK/stray-leave.txt" SOT_SOCKET="$SOCK" "$SCRIPTS_DIR/comm-leave.sh" --name "$DH" 2>&1)" || rc=$?
    stop_stub_daemon
    [ "$rc" -eq 0 ] || { echo "  leave exit $rc: $out"; return 1; }
    jq -e --arg n "$DH" '.agents | has($n) | not' "$REG" >/dev/null || { echo "  @$DH was not removed"; return 1; }
}
case_d6_an_unbound_entry_is_asked_about_too() {
    seed_derived_bound || return 1
    jq --arg n "$DH" '.agents[$n].workspace_id = ""' "$REG" > "$REG.new" && mv "$REG.new" "$REG"
    held_by "$DH"; start_stub_daemon
    : > "$REQLOG"
    SOT_SOCKET="$SOCK" stray_join --name "$DH"
    local asked; asked="$(grep -c "\"handle\":\"$DH\"" "$REQLOG")"
    stop_stub_daemon
    [ "$JOIN_RC" -eq 1 ] || { echo "  exit $JOIN_RC: $JOIN_OUT"; return 1; }
    case "$JOIN_OUT" in *"@$DH is held by a row"*) ;; *) echo "  message: $JOIN_OUT"; return 1 ;; esac
    [ "$asked" -ge 1 ] || { echo "  the daemon was never asked about @$DH"; return 1; }
}
case_d7_no_daemon_and_an_unbound_entry_is_taken() {
    seed_derived_bound || return 1
    jq --arg n "$DH" '.agents[$n].workspace_id = ""' "$REG" > "$REG.new" && mv "$REG.new" "$REG"
    SOT_SOCKET="$WORK/no-daemon.sock" stray_join --name "$DH"
    [ "$JOIN_RC" -eq 0 ] || { echo "  join failed: $JOIN_OUT"; return 1; }
}
case_d8_a_row_that_binds_in_the_gap_keeps_the_handle() {
    local h="t-gap"
    held_by ""; printf '%s\n' "$h" > "$WORK/bind-on-ask.txt"; start_stub_daemon
    SOT_SOCKET="$SOCK" stray_join --name "$h"
    stop_stub_daemon; rm -f "$WORK/bind-on-ask.txt"
    [ "$JOIN_RC" -ne 0 ] || { echo "  join succeeded over a row that bound in the gap: $JOIN_OUT"; return 1; }
    [ "$(jq -r --arg n "$h" '.agents[$n].workspace_id' "$REG")" = "row-late" ] || { echo "  the row's entry was overwritten"; return 1; }
    # derived: the tier the stub binds in the gap is stepped past, not overwritten
    jq --arg n "$h" 'del(.agents[$n])' "$REG" > "$REG.new" && mv "$REG.new" "$REG"
    SOT_SOCKET="" stray_join; local t1; t1="$(sed -n 1p "$STRAY_SELF")"
    jq --arg n "$t1" 'del(.agents[$n])' "$REG" > "$REG.new" && mv "$REG.new" "$REG"
    printf '%s\n' "$t1" > "$WORK/bind-on-ask.txt"; start_stub_daemon
    SOT_SOCKET="$SOCK" stray_join
    stop_stub_daemon; rm -f "$WORK/bind-on-ask.txt"
    [ "$JOIN_RC" -eq 0 ] || { echo "  derived join failed: $JOIN_OUT"; return 1; }
    [ "$(sed -n 1p "$STRAY_SELF")" != "$t1" ] || { echo "  derived join took @$t1 over a row that bound in the gap"; return 1; }
    [ "$(jq -r --arg n "$t1" '.agents[$n].workspace_id' "$REG")" = "row-late" ] || { echo "  the row's entry for @$t1 was overwritten"; return 1; }
}
case_d9_an_entry_bound_on_another_computer_is_held() {
    local h="t-far"
    jq --arg n "$h" '.agents[$n] = {host:"otherbox", workspace_id:"row-far", repo:"r", root:"/x", status:"idle"}' "$REG" > "$REG.new" && mv "$REG.new" "$REG"
    held_by ""; start_stub_daemon
    local before; before="$(cat "$REG")"
    SOT_SOCKET="$SOCK" stray_join --name "$h"
    stop_stub_daemon
    [ "$JOIN_RC" -eq 1 ] || { echo "  exit $JOIN_RC: $JOIN_OUT"; return 1; }
    case "$JOIN_OUT" in *"@$h"*"another computer"*) ;; *) echo "  message: $JOIN_OUT"; return 1 ;; esac
    [ "$(cat "$REG")" = "$before" ] || { echo "  registry changed"; return 1; }
}
case_d10_an_unreadable_registry_refuses_a_join_from_outside() {
    cp "$REG" "$WORK/registry.saved"; : > "$REG"
    held_by ""; start_stub_daemon
    SOT_SOCKET="$SOCK" stray_join --name "t-unreadable"
    stop_stub_daemon
    cp "$WORK/registry.saved" "$REG"
    [ "$JOIN_RC" -ne 0 ] || { echo "  join succeeded: $JOIN_OUT"; return 1; }
    case "$JOIN_OUT" in *"registry could not be read"*) ;; *) echo "  message: $JOIN_OUT"; return 1 ;; esac
}
case_d11_a_derived_join_takes_no_tier_the_daemon_holds() {
    # Tier 1's entry is absent and nothing was cleared: the case where bash 3.2 dropped the empty CLEARED word and
    # `_sot_tier_claimable` skipped the clearance check.
    cp "$WORK/registry.seed" "$REG"
    : > "$WORK/held.txt"; : > "$WORK/held-all"; start_stub_daemon
    local before; before="$(cat "$REG")"
    SOT_SOCKET="$SOCK" stray_join
    stop_stub_daemon; rm -f "$WORK/held-all"
    [ "$JOIN_RC" -ne 0 ] || { echo "  a derived join took @$(sed -n 1p "$STRAY_SELF") with every tier held: $JOIN_OUT"; return 1; }
    [ "$(cat "$REG")" = "$before" ] || { echo "  registry changed"; return 1; }
}
case_d14_the_clearance_alone_finds_the_registry_unreadable() {
    # The clearance's own read says unreadable (sot_row_binding, stubbed in a copy of the scripts) while the writer
    # reads the registry fine, so only the clearance's refusal can stop this join.
    local copy="$WORK/scripts-unreadable" before out rc=0
    rm -rf "${copy:?}"; cp -r "$SCRIPTS_DIR" "$copy" && chmod -R u+w "$copy" || return 1
    printf '\nsot_row_binding() { echo unreadable; }\n' >> "$copy/comm-lib.sh"
    cp "$WORK/registry.seed" "$REG"; before="$(cat "$REG")"
    held_by ""; start_stub_daemon
    out="$(cd "$WORK" && env SOT_COMM_SELF_FILE="$WORK/stray-ur.txt" SOT_SOCKET="$SOCK" "$copy/comm-join.sh" --name t-unreadable-ruling 2>&1)" || rc=$?
    stop_stub_daemon
    [ "$rc" -ne 0 ] || { echo "  join succeeded: $out"; return 1; }
    case "$out" in *"FAILED"*"registry could not be read"*) ;; *) echo "  message: $out"; return 1 ;; esac
    [ "$(cat "$REG")" = "$before" ] || { echo "  registry changed"; return 1; }
}
case_bind_row_binding_states() {
    bind_row "$ROW"
    local got
    got="$(bash -c 'source "$1/comm-lib.sh"; for a in "$2 $3" "$2 other-row" "$2 ''" "t-nobody $3"; do set -- $a; sot_row_binding "$1" "${2:-}" | cut -f1; done' _ "$SCRIPTS_DIR" "$NAME" "$ROW" | paste -sd ' ' -)"
    [ "$got" = "owner other other unbound" ] || { echo "  states: '$got'"; return 1; }
    cp "$REG" "$WORK/registry.bind"; : > "$REG"
    got="$(bash -c 'source "$1/comm-lib.sh"; sot_row_binding "$2" ""' _ "$SCRIPTS_DIR" "$NAME")"
    cp "$WORK/registry.bind" "$REG"
    [ "$got" = unreadable ] || { echo "  unreadable registry gave '$got'"; return 1; }
}
case_s1_a_stray_whose_self_file_names_a_held_handle() {
    bind_row ""; reset_inbox
    jq -nc --arg to "$NAME" '{from:"peer",to:$to,repo:"r",msg:"held-row-mail",ts:"2026-01-01T00:00:02Z"}' > "$INBOX"   # what only @$NAME's reader sees
    held_by "$NAME"; start_stub_daemon
    local self="$WORK/stray-s1.txt" out
    cp "$SELF" "$self"
    out="$(cd "$WORK" && env SOT_COMM_SELF_FILE="$self" SOT_SOCKET="$SOCK" HOME="$WORK" "$SCRIPTS_DIR/comm-session-start.sh" 2>&1)"
    stop_stub_daemon
    case "$out" in *"BOOTSTRAP-ARM handle="*"identity=ok"*) ;; *) echo "  no identity=ok: $out"; return 1 ;; esac
    case "$out" in *"handle=$NAME "*) echo "  kept @$NAME: $out"; return 1 ;; esac
    [ "$(sed -n 1p "$self")" != "$NAME" ] || { echo "  the self file still names @$NAME"; return 1; }
    poll "$self"
    case "$POLL_OUT" in *held-row-mail*) echo "  the session still reads @$NAME's inbox: $POLL_OUT"; return 1 ;; esac
    [ "$(cat "$CURSOR")" = "0" ] || { echo "  @$NAME's cursor moved to '$(cat "$CURSOR")'"; return 1; }
}
case_s3_a_displaced_row_rejoins() {
    local ra=row-a rb=row-b self_a self_b out
    self_a="$WORK/${HOST}__$ra.txt"; self_b="$WORK/${HOST}__$rb.txt"; rm -f "$self_a" "$self_b"
    ( cd "$WORK" && in_row "$ra" env SOT_COMM_SELF_FILE="$self_a" SOT_WORKSPACE_ID="$ra" "$JOIN" ) >/dev/null 2>&1 || { echo "  row-a join failed"; return 1; }
    local h; h="$(sed -n 1p "$self_a")"
    ( cd "$WORK" && in_row "$rb" env SOT_COMM_SELF_FILE="$self_b" SOT_WORKSPACE_ID="$rb" "$JOIN" ) >/dev/null 2>&1 || { echo "  row-b join failed"; return 1; }
    [ "$(sed -n 1p "$self_b")" = "$h" ] || { echo "  row-b did not reclaim @$h (premise)"; return 1; }
    [ "$(jq -r --arg n "$h" '.agents[$n].workspace_id' "$REG")" = "$rb" ] || { echo "  premise: registry not row-b's"; return 1; }
    out="$(cd "$WORK" && in_row "$ra" env SOT_COMM_SELF_FILE="$self_a" SOT_WORKSPACE_ID="$ra" HOME="$WORK" "$SCRIPTS_DIR/comm-session-start.sh" 2>&1)"
    case "$out" in *REFUSED*|*"identity=FAIL"*) echo "  refused: $out"; return 1 ;; esac
    case "$out" in *"@$h moved to row $rb"*) ;; *) echo "  no moved line: $out"; return 1 ;; esac
    case "$out" in *"BOOTSTRAP-ARM handle=$h identity=ok"*) ;; *) echo "  no identity=ok: $out"; return 1 ;; esac
    [ "$(jq -r --arg n "$h" '.agents[$n].workspace_id' "$REG")" = "$ra" ] || { echo "  the handle did not move back to $ra"; return 1; }
}

check "a: a stray session in the folder is refused, cursor untouched" case_a_stray_session
check "a2: the stray's Stop hook does not block its turn end" case_a2_stray_stop_hook_does_not_block
check "e: a respawned row rejoins and polls as the new row" case_e_respawned_row_rejoins
check "b: a process of another row is refused, cursor untouched" case_b_other_row
check "b3: an id exported over a private self-file is no row id, so it is refused" case_b3_an_exported_id_over_a_private_file_is_no_row_id
check "b2: a process carrying the row's id reads and advances" case_b2_own_row_reads
check "c: the nopane slot naming a row's handle is refused" case_c_nopane_slot
check "ctl: a handle with no row polls with no row id" case_ctl_no_row_polls
check "g: an unreadable registry reads nothing and moves no cursor" case_g_unreadable_registry_reads_nothing
check "r: --rebind from a process with no row id rebinds nothing" case_r_rebind_needs_a_row_id
check "d: a stray's plain join steps past a handle the daemon says a row holds" case_d_stray_join_steps_past_a_held_handle
check "d2: a stray's --name of a held handle is refused with the daemon's message" case_d2_stray_name_of_a_held_handle_is_refused
check "d3: a stray takes a bound handle whose row the daemon says is gone" case_d3_stray_takes_a_handle_whose_row_is_gone
check "d4: a stray's --name of a bound handle is refused when no daemon can be asked" case_d4_stray_name_of_a_bound_handle_with_no_daemon_is_refused
check "ds: a stray's session start steps past the held handle, identity ok" case_ds_stray_session_start_steps_past
check "d5: leave then join of a held handle is refused and the daemon was asked" case_d5_leave_then_join_is_refused
check "d12: with no daemon, comm-leave.sh --name of a bound entry is refused" case_d12_no_daemon_leave_of_a_bound_entry_is_refused
check "d13: comm-leave.sh --name of an orphan the daemon does not hold removes it" case_d13_leave_of_an_orphan_the_daemon_does_not_hold_removes_it
check "d6: an entry bound to no row is asked about too" case_d6_an_unbound_entry_is_asked_about_too
check "d7: no daemon and an entry bound to no row: taken (the named limit)" case_d7_no_daemon_and_an_unbound_entry_is_taken
check "d8: a row that binds the handle in the gap keeps it" case_d8_a_row_that_binds_in_the_gap_keeps_the_handle
check "d9: an entry bound on another computer is held" case_d9_an_entry_bound_on_another_computer_is_held
check "d10: an unreadable registry refuses a join from outside" case_d10_an_unreadable_registry_refuses_a_join_from_outside
check "d11: a derived join with every tier held by the daemon takes none, whatever bash" case_d11_a_derived_join_takes_no_tier_the_daemon_holds
check "d14: a registry only the clearance finds unreadable refuses a join from outside" case_d14_the_clearance_alone_finds_the_registry_unreadable
check "bind: sot_row_binding says owner, other, unbound and unreadable" case_bind_row_binding_states
check "s1: a stray whose self file names a held handle starts as its own handle" case_s1_a_stray_whose_self_file_names_a_held_handle
check "s3: a displaced row's session start rejoins and says it moved" case_s3_a_displaced_row_rejoins

echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
