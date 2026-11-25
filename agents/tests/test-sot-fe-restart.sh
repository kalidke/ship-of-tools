#!/usr/bin/env bash
# test-sot-fe-restart.sh — hermetic suite for `sot-fe restart`. The verb starts a Codex
# session's agent again through one `workspace.restart`, naming the session as typed. It
# is never the calling session's own (`.` or its own row id), refused before an endpoint
# is resolved with nothing sent; it takes exactly one argument; each refusal of the
# daemon prints its code and words and exits 2; a restart exits 0; a stop or a start that
# did not finish, and no answer, exit 3; and the one request is never sent twice.
#
# No real daemon: a STUB unix-socket daemon (the same harness as
# test-sot-fe-version.sh) answers `workspace.restart` with a canned reply and
# logs every request line, so each case can count what was sent. Never touches
# a real ~/.sot-comm (a temp $SOT_COMM_HOME) or any real daemon socket.
#
# Usage: agents/tests/test-sot-fe-restart.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-wait.sh" || exit 2

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-fe-restart-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi

export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
export SOTD_BIN="$(guard_bridge_stub "$WORK/bridge")"
[ -x "$SOTD_BIN" ] || exit 2
SOT_FE="$SCRIPTS_DIR/sot-fe"
mkdir -p "$SOT_COMM_HOME"

STUB_NC_PID=""
STUB_WATCHER_PID=""
cleanup() {
    end_group "$STUB_WATCHER_PID"
    end_group "$STUB_NC_PID"
    { exec 3>&-; } 2>/dev/null || true
    rm -rf "${WORK:?}"
}
trap cleanup EXIT

PASS=0
FAIL=0
SKIP=0

# check DESC FN — mirrors test-join-disambiguation.sh's own runner exactly:
# FN prints diagnostics and returns 0 = pass, 2 = SKIP, anything else = fail.
check() {
    local desc="$1" fn="$2"
    local rc
    "$fn"
    rc=$?
    case "$rc" in
        0) echo "PASS: $desc"; PASS=$((PASS + 1)) ;;
        2) echo "SKIP: $desc"; SKIP=$((SKIP + 1)) ;;
        *) echo "FAIL: $desc"; FAIL=$((FAIL + 1)) ;;
    esac
}

contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# --- the stub daemon -----------------------------------------------------
#
# stage_reply OP JSON_LINE — pre-stage the canned reply the watcher sends
# back the next time it sees a request whose `.op` is OP. Must be called
# BEFORE start_stub_daemon (the watcher script embeds the staged replies at
# start time — this suite's cases are all "decide the whole conversation up
# front", never mid-flight reconfiguration).
declare -A STAGED_REPLY
stage_reply() {
    STAGED_REPLY["$1"]="$2"
}

# start_stub_daemon — bind $SOCK (fresh path each call) and arm the
# content-addressed watcher over the currently-staged replies. Blocks
# briefly to let nc actually bind before any caller connects.
SOCK=""
STUBN=0
start_stub_daemon() {
    STUBN=$((STUBN + 1))
    SOCK="$WORK/stub-$STUBN.sock"
    local fifo="$WORK/resp-$STUBN.fifo"
    local reqlog="$WORK/req-$STUBN.log"
    local repliesdir="$WORK/replies-$STUBN"
    mkfifo "$fifo"
    : > "$reqlog"
    mkdir -p "$repliesdir"

    # Freeze the currently-staged replies as plain files, one per op —
    # a DIRECT write, never shell-quoted code generation, so a reply
    # containing quotes/backslashes (real JSON) can never break the
    # backgrounded watcher below.
    local op
    for op in "${!STAGED_REPLY[@]}"; do
        printf '%s' "${STAGED_REPLY[$op]}" > "$repliesdir/$op.json"
    done

    # Keep a writer fd open on the fifo for nc's whole `-k` lifetime (see
    # this file's header doc) — opened BEFORE nc so nc's own open-for-read
    # never blocks waiting for a first writer.
    exec 3<>"$fifo"
    trap - EXIT; set -m   # each step leads a process group of its own, which end_group (lib-wait.sh) ends whole
    nc -klU "$SOCK" < "$fifo" >> "$reqlog" &
    STUB_NC_PID=$!

    ( tail -n +1 -F "$reqlog" 2>/dev/null | while IFS= read -r line; do
        op="$(printf '%s' "$line" | jq -r '.op // empty' 2>/dev/null)"
        replyfile="$repliesdir/$op.json"
        [ -n "$op" ] && [ -f "$replyfile" ] && printf '%s\n' "$(cat "$replyfile")" >&3
    done ) &
    STUB_WATCHER_PID=$!
    set +m; trap cleanup EXIT   # the steps are forked with no EXIT trap: a stop that reaches one before bash resets the copy would run cleanup in it

    # Bounded wait for the socket to actually exist (nc binds it near-
    # instantly; this just guards a slow-scheduling CI box).
    await test -S "$SOCK" || echo "stub daemon socket never appeared: $SOCK" >&2
}

stop_stub_daemon() {
    # The watcher is a subshell whose `tail -F | while` members outlive a
    # kill of the subshell alone and keep this script's stdout open — a
    # caller piping the suite (`| tail`) then never sees EOF — so each step
    # ends as the whole process group it leads.
    end_group "$STUB_WATCHER_PID"
    end_group "$STUB_NC_PID"
    { exec 3>&-; } 2>/dev/null || true
    STUB_NC_PID=""
    STUB_WATCHER_PID=""
    STAGED_REPLY=()
}

# --- helpers ---------------------------------------------------------------

# run_restart ENVSPEC... -- ARGS...: run sot-fe restart against the stub with only the given SOT_ vars.
RS_OUT=""; RS_RC=0
run_restart() {
    local envs=()
    while [ "$1" != "--" ]; do envs+=("$1"); shift; done; shift
    RS_OUT="$(env -u SOT_WORKSPACE_ID "${envs[@]}" "$SOT_FE" restart "$@" --endpoint "unix:$SOCK" --timeout 3 2>&1)"
    RS_RC=$?
}
sent_restart() { grep -c '"op":"workspace.restart"' "$WORK/req-$STUBN.log"; }
sent_any() { grep -c '"kind":"req"' "$WORK/req-$STUBN.log"; }

# reply JSON: the staged answer of the daemon for a restart.
reply() { printf '{"v":1,"id":2,"kind":"res","op":"workspace.restart","payload":%s}' "$1"; }
LOOK="Look at the session before you run this again: a second run starts another new conversation."
RESTARTED='{"outcome":"restarted","slug":"alpha","workspace_id":"ws-alpha-1","detail":""}'

# --- cases -----------------------------------------------------------------

case_usage_takes_exactly_one_argument() {
    stage_reply workspace.restart "$(reply "$RESTARTED")"; start_stub_daemon
    run_restart --; local none_rc=$RS_RC none_out=$RS_OUT
    run_restart -- alpha beta; local two_rc=$RS_RC
    local n; n="$(sent_any)"; stop_stub_daemon
    [ "$none_rc" = 1 ] && [ "$two_rc" = 1 ] && [ "$n" = 0 ] \
        && contains "$none_out" "usage: sot-fe restart <session> [--timeout <s>]  -- name the session as sot-fe version lists it, or by its id" \
        || { echo "rc=$none_rc/$two_rc sent=$n out=$none_out"; return 1; }
}

case_the_callers_own_session_is_refused_before_anything_is_sent() {
    stage_reply workspace.restart "$(reply "$RESTARTED")"; start_stub_daemon
    local want="ERROR: a session cannot restart its own agent. Ask its person to run this in another session or a shell on this computer. Nothing was sent."
    run_restart SOT_WORKSPACE_ID=ws-self-1a2b -- .;            local a_rc=$RS_RC a_out=$RS_OUT
    run_restart SOT_WORKSPACE_ID=ws-self-1a2b -- ws-self-1a2b; local b_rc=$RS_RC b_out=$RS_OUT
    run_restart -- .;                                          local c_rc=$RS_RC c_out=$RS_OUT
    local n; n="$(sent_any)"; stop_stub_daemon
    [ "$a_rc" = 2 ] && [ "$b_rc" = 2 ] && [ "$c_rc" = 2 ] && [ "$n" = 0 ] \
        && [ "$a_out" = "$want" ] && [ "$b_out" = "$want" ] && [ "$c_out" = "$want" ] \
        || { echo "rc=$a_rc/$b_rc/$c_rc sent=$n out=$a_out"; return 1; }
}

case_one_request_carries_the_session_as_typed() {
    stage_reply workspace.restart "$(reply "$RESTARTED")"; start_stub_daemon
    run_restart SOT_WORKSPACE_ID=ws-self-1a2b -- '/odd slug'
    local n s b
    n="$(sent_restart)"
    s="$(jq -r 'select(.op=="workspace.restart") | .payload.session' "$WORK/req-$STUBN.log")"
    b="$(jq -r 'select(.op=="workspace.restart") | .payload.by' "$WORK/req-$STUBN.log")"
    stop_stub_daemon
    [ "$n" = 1 ] && [ "$s" = '/odd slug' ] && [ "$b" = ws-self-1a2b ] || { echo "sent=$n session=$s by=$b out=$RS_OUT"; return 1; }
}

# Outside a session the request names no `by`; inside one it names the row, so the daemon can refuse the caller's own
# row by any name its slug was given.
case_a_caller_outside_a_session_sends_no_by() {
    stage_reply workspace.restart "$(reply "$RESTARTED")"; start_stub_daemon
    run_restart -- alpha
    local n has
    n="$(sent_restart)"
    has="$(jq -r 'select(.op=="workspace.restart") | .payload | has("by")' "$WORK/req-$STUBN.log")"
    stop_stub_daemon
    [ "$n" = 1 ] && [ "$has" = false ] || { echo "sent=$n has_by=$has out=$RS_OUT"; return 1; }
}

case_a_restart_prints_its_line_and_exits_0() {
    stage_reply workspace.restart "$(reply "$RESTARTED")"; start_stub_daemon
    run_restart -- alpha
    local n; n="$(sent_restart)"; stop_stub_daemon
    [ "$RS_RC" = 0 ] && [ "$n" = 1 ] \
        && [ "$RS_OUT" = "restarted alpha: its Codex agent was started again under this computer's Ship of Tools, in a new conversation that begins by reading its handoff and its mail. Look at its mark again in the window or in sotd status: if it still reads not woken by mail, this computer's install does not start Codex with its hooks, so run the installer on this computer and check that its Codex accepts --no-daemon. Do not run this again for that: it ends another conversation and changes nothing." ] \
        || { echo "rc=$RS_RC sent=$n out=$RS_OUT"; return 1; }
}

# A refusal's code and the daemon's words: `refused <code>: <words> Nothing was restarted.`, exit 2, one request.
REFUSALS='unknown_workspace|this computer runs no session named alpha.
own_session|a session cannot restart its own agent. Ask its person to run this in another session or a shell on this computer.
not_codex|alpha runs claude, not Codex; only a Codex session'"'"'s agent is started again this way.
not_ready|alpha is not running (stopped). Open it in the window to start it, and run this again if it is still marked not woken by mail.
not_needed|alpha is not marked not woken by mail, so a restart is not needed and would end its conversation.
busy|alpha is in a turn. Run this again when the turn has ended.
registry_unreadable|whether alpha is in a turn could not be told (the comm registry is not valid JSON: x), so its agent was left as it is.
in_progress|alpha is being started or ended already. Run this again in a moment.
closing|this computer'"'"'s Ship of Tools is closing.
launcher_unresolved|no launcher found'
case_each_refusal_prints_its_code_and_words_and_exits_2() {
    local code words want n bad=0
    while IFS='|' read -r code words; do
        [ -n "$code" ] || continue
        stage_reply workspace.restart "$(reply "$(jq -nc --arg e "$words" --arg c "$code" '{error:$e, code:$c}')")"; start_stub_daemon
        run_restart -- alpha
        n="$(sent_restart)"; stop_stub_daemon
        want="refused $code: ${words%.}. Nothing was restarted."
        [ "$RS_RC" = 2 ] && [ "$RS_OUT" = "$want" ] && [ "$n" = 1 ] || { echo "$code: rc=$RS_RC sent=$n out=$RS_OUT"; bad=1; }
    done <<<"$REFUSALS"
    [ "$bad" = 0 ]
}

case_an_older_daemons_unknown_op_is_unsupported() {
    stage_reply workspace.restart "$(reply '{"error":"unknown op: workspace.restart"}')"; start_stub_daemon
    run_restart -- alpha
    local n; n="$(sent_restart)"; stop_stub_daemon
    [ "$RS_RC" = 2 ] && [ "$n" = 1 ] \
        && [ "$RS_OUT" = "refused unsupported: the computer that runs your sessions runs an older Ship of Tools that cannot restart a session's agent. Nothing was restarted." ] \
        || { echo "rc=$RS_RC sent=$n out=$RS_OUT"; return 1; }
}

case_a_stop_that_did_not_finish_and_a_start_that_did_not_happen_exit_3() {
    local bad=0 n
    stage_reply workspace.restart "$(reply '{"outcome":"stop_failed","slug":"alpha","workspace_id":"ws-alpha-1","detail":"the supervisor did not stop"}')"; start_stub_daemon
    run_restart -- alpha; n="$(sent_restart)"; stop_stub_daemon
    [ "$RS_RC" = 3 ] && [ "$n" = 1 ] \
        && [ "$RS_OUT" = "unknown: alpha's agent may still be the one it had: its stop did not finish (the supervisor did not stop). $LOOK" ] \
        || { echo "stop_failed: rc=$RS_RC sent=$n out=$RS_OUT"; bad=1; }
    stage_reply workspace.restart "$(reply '{"outcome":"not_started","slug":"alpha","workspace_id":"ws-alpha-1","detail":"spawn failed"}')"; start_stub_daemon
    run_restart -- alpha; n="$(sent_restart)"; stop_stub_daemon
    [ "$RS_RC" = 3 ] && [ "$n" = 1 ] \
        && [ "$RS_OUT" = "not started: alpha's Codex program ended and was not started again (spawn failed). Opening the session in the window starts it." ] \
        || { echo "not_started: rc=$RS_RC sent=$n out=$RS_OUT"; bad=1; }
    [ "$bad" = 0 ]
}

case_no_answer_is_one_request_and_exit_3() {
    start_stub_daemon   # nothing staged: the stub never answers
    run_restart -- alpha
    local n; n="$(sent_restart)"; stop_stub_daemon
    [ "$RS_RC" = 3 ] && [ "$n" = 1 ] \
        && [ "$RS_OUT" = "no answer: the daemon did not answer within 3 s, so alpha's agent may have been started again. $LOOK" ] \
        || { echo "rc=$RS_RC sent=$n out=$RS_OUT"; return 1; }
}

check "a restart takes exactly one argument and prints its usage"                         case_usage_takes_exactly_one_argument
check "the calling session's own is refused before anything is sent"                      case_the_callers_own_session_is_refused_before_anything_is_sent
check "one request carries the session as typed and the caller's own row as by"                                          case_one_request_carries_the_session_as_typed
check "a caller outside a session sends no by"                                            case_a_caller_outside_a_session_sends_no_by
check "a restart prints its line and exits 0"                                             case_a_restart_prints_its_line_and_exits_0
check "each refusal prints its code and words and exits 2"                                case_each_refusal_prints_its_code_and_words_and_exits_2
check "an older daemon's unknown-op answer is unsupported"                                case_an_older_daemons_unknown_op_is_unsupported
check "a stop that did not finish and a start that did not happen exit 3"                 case_a_stop_that_did_not_finish_and_a_start_that_did_not_happen_exit_3
check "no answer is one request and exit 3"                                               case_no_answer_is_one_request_and_exit_3

echo ""
echo "$PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
