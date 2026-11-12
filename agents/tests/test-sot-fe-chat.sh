#!/usr/bin/env bash
# test-sot-fe-chat.sh — hermetic suite for `sot-fe chat post`, a row's reply in
# its channel. The verb posts only for the row it runs in ($SOT_WORKSPACE_ID)
# under that session's comm handle: it asks `chat.resolve` for the pair, sends
# ONE `chat.post` of kind `row` into the thread, and never a second (a post
# that may exist is not sent again, so no answer is one request, not two).
# Outside a row, or with no handle, it is refused before an endpoint is found.
# Exit codes: 0 posted; 1 usage; 2 nothing was posted (refused, limited); 3 the post may exist (unknown, no answer).
#
# No real daemon: a STUB unix-socket daemon (the harness of
# test-sot-fe-reauth.sh) answers `chat.resolve` and `chat.post` with canned
# replies and logs every request line, so each case can count what was sent.
# The session's comm handle is a stand-in comm-context.sh in the staged copy.
# Never touches a real comm folder (a temp $SOT_COMM_HOME) or any real daemon
# socket.
#
# Usage: agents/tests/test-sot-fe-chat.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-wait.sh" || exit 2

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-fe-chat-test-XXXXXX")"
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
# The handle the session derives is the case's to set (STUB_NAME; empty is a session that has not joined).
printf '%s\n' '#!/usr/bin/env bash' 'printf "NAME=%s\n" "${STUB_NAME:-}"' > "$SCRIPTS_DIR/comm-context.sh"
chmod +x "$SCRIPTS_DIR/comm-context.sh"

# A jq on PATH that logs its argv (NUL-separated, to $JQ_ARGV_LOG when a case sets it) and runs the real one, so a case
# can assert the free text was never on a jq command line.
REAL_JQ="$(command -v jq)"; export REAL_JQ
mkdir -p "$WORK/shim"
printf '%s\n' '#!/usr/bin/env bash' '[ -z "${JQ_ARGV_LOG:-}" ] || printf "%s\0" "$@" >> "$JQ_ARGV_LOG"' 'exec "$REAL_JQ" "$@"' > "$WORK/shim/jq"
chmod +x "$WORK/shim/jq"
PATH="$WORK/shim:$PATH"

STUB_NC_PID=""
STUB_WATCHER_PID=""
cleanup() {
    end_group "$STUB_WATCHER_PID"
    end_group "$STUB_NC_PID"
    exec 3>&- 2>/dev/null || true
    rm -rf "${WORK:?}"
}
trap cleanup EXIT

PASS=0
FAIL=0
SKIP=0

# check DESC FN — the runner of the other agents suites:
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
# BEFORE start_stub_daemon (the watcher embeds the staged replies at start
# time — every case decides the whole conversation up front).
declare -A STAGED_REPLY
stage_reply() {
    STAGED_REPLY["$1"]="$2"
}

# start_stub_daemon — bind $SOCK (fresh path each call) and arm the
# content-addressed watcher over the currently-staged replies.
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

    # Freeze the currently-staged replies as plain files, one per op — a
    # DIRECT write, never shell-quoted code generation.
    local op
    for op in "${!STAGED_REPLY[@]}"; do
        printf '%s' "${STAGED_REPLY[$op]}" > "$repliesdir/$op.json"
    done

    # Keep a writer fd open on the fifo for nc's whole `-k` lifetime, opened
    # BEFORE nc so nc's own open-for-read never blocks on a first writer.
    exec 3<>"$fifo"
    set -m   # each step leads a process group of its own, which end_group (lib-wait.sh) ends whole
    nc -klU "$SOCK" < "$fifo" >> "$reqlog" &
    STUB_NC_PID=$!

    ( tail -n +1 -F "$reqlog" 2>/dev/null | while IFS= read -r line; do
        op="$(printf '%s' "$line" | jq -r '.op // empty' 2>/dev/null)"
        replyfile="$repliesdir/$op.json"
        [ -n "$op" ] && [ -f "$replyfile" ] && printf '%s\n' "$(cat "$replyfile")" >&3
    done ) &
    STUB_WATCHER_PID=$!
    set +m

    await test -S "$SOCK" || echo "stub daemon socket never appeared: $SOCK" >&2
}

stop_stub_daemon() {
    end_group "$STUB_WATCHER_PID"
    end_group "$STUB_NC_PID"
    exec 3>&- 2>/dev/null || true
    STUB_NC_PID=""
    STUB_WATCHER_PID=""
    STAGED_REPLY=()
}

# --- helpers ---------------------------------------------------------------

# run_chat STDIN ENVSPEC... -- ARGS...: run `sot-fe chat post ARGS` against the stub with only the given variables
# and STDIN as its standard input.
CH_OUT=""; CH_RC=0
run_chat() {
    local input="$1" envs=()
    shift
    while [ "$1" != "--" ]; do envs+=("$1"); shift; done; shift
    CH_OUT="$(printf '%s' "$input" | env -u SOT_WORKSPACE_ID -u STUB_NAME JQ_ARGV_LOG="$WORK/jq-argv.log" "${envs[@]}" "$SOT_FE" chat post "$@" --endpoint "unix:$SOCK" --timeout 3 2>&1)"
    CH_RC=$?
}
# How many requests of OP the stub has logged; and how many requests of any op (hellos included).
sent() { grep -c "\"op\":\"$1\"" "$WORK/req-$STUBN.log"; }
sent_any() { grep -c '"kind":"req"' "$WORK/req-$STUBN.log"; }
# One field of the logged chat.post.
posted_field() { jq -r "select(.op==\"chat.post\") | $1" "$WORK/req-$STUBN.log"; }

ROW=ws-self-1a2b
MAPPED='{"v":1,"id":2,"kind":"res","op":"chat.resolve","payload":{"state":"mapped","workspace":"T0AAAA","channel":"C0AAAA","delivers":true}}'
POSTED='{"v":1,"id":3,"kind":"res","op":"chat.post","payload":{"outcome":"posted","ts":"1700000000.000100"}}'

# The row mapped and the post answered with the JSON object $1; the verb run with a thread and a plain text.
answered() {
    stage_reply chat.resolve "$MAPPED"
    stage_reply chat.post "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.post\",\"payload\":$1}"
    start_stub_daemon
    run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- --thread 1700000050.000000 "done"
}

# --- cases -----------------------------------------------------------------

case_outside_a_row_is_refused_before_an_endpoint() {
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.post "$POSTED"; start_stub_daemon
    run_chat "" STUB_NAME=alpha-h -- "hi"
    local n; n="$(sent_any)"; stop_stub_daemon
    [ "$CH_RC" = 2 ] && contains "$CH_OUT" "SOT_WORKSPACE_ID is unset" && [ "$n" = 0 ] \
        || { echo "rc=$CH_RC sent=$n out=$CH_OUT"; return 1; }
}

case_no_handle_is_refused_before_an_endpoint() {
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.post "$POSTED"; start_stub_daemon
    run_chat "" SOT_WORKSPACE_ID=$ROW -- "hi"
    local n; n="$(sent_any)"; stop_stub_daemon
    [ "$CH_RC" = 2 ] && contains "$CH_OUT" "no comm handle" && [ "$n" = 0 ] \
        || { echo "rc=$CH_RC sent=$n out=$CH_OUT"; return 1; }
}

case_one_resolve_then_one_row_post_with_handle_and_thread() {
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.post "$POSTED"; start_stub_daemon
    run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- --thread 1700000050.000000 "done"
    local r p first rw k n t w c h x
    r="$(sent chat.resolve)"; p="$(sent chat.post)"
    first="$(jq -r 'select(.op=="chat.resolve" or .op=="chat.post") | .op' "$WORK/req-$STUBN.log" | head -n 1)"
    rw="$(jq -r 'select(.op=="chat.resolve") | .payload.row' "$WORK/req-$STUBN.log")"
    k="$(posted_field .payload.kind)"; n="$(posted_field .payload.name)"; t="$(posted_field .payload.thread)"
    w="$(posted_field .payload.target.workspace)"; c="$(posted_field .payload.target.channel)"
    h="$(posted_field .payload.target.row)"; x="$(posted_field .payload.text)"
    stop_stub_daemon
    [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "posted 1700000000.000100" ] && [ "$r" = 1 ] && [ "$p" = 1 ] && [ "$first" = chat.resolve ] \
        && [ "$rw" = "$ROW" ] && [ "$k" = row ] && [ "$n" = alpha-h ] && [ "$t" = 1700000050.000000 ] \
        && [ "$w" = T0AAAA ] && [ "$c" = C0AAAA ] && [ "$h" = "$ROW" ] && [ "$x" = done ] \
        || { echo "rc=$CH_RC out=$CH_OUT resolve=$r post=$p first=$first row=$rw kind=$k name=$n thread=$t ws=$w ch=$c target_row=$h text=$x"; return 1; }
}

case_without_a_thread_the_post_carries_none() {
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.post "$POSTED"; start_stub_daemon
    run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- "done"
    local has; has="$(posted_field '.payload | has("thread")')"; stop_stub_daemon
    [ "$CH_RC" = 0 ] && [ "$has" = false ] || { echo "rc=$CH_RC has_thread=$has out=$CH_OUT"; return 1; }
}

case_each_outcome_has_its_words_and_exit_code() {
    local bad="" want_rc want_text payload
    while IFS='|' read -r payload want_rc want_text; do
        answered "$payload"
        stop_stub_daemon
        { [ "$CH_RC" = "$want_rc" ] && [ "$CH_OUT" = "$want_text" ]; } \
            || bad="$bad [$payload: rc=$CH_RC out=$CH_OUT]"
    done <<'EOF'
{"outcome":"posted","ts":"1700000000.000100"}|0|posted 1700000000.000100
{"outcome":"refused","code":"busy"}|2|refused busy the chat service is busy
{"outcome":"refused","code":"bad_request"}|2|refused bad_request the request is not valid (empty or over-long text, or a name that is not this row's)
{"outcome":"limited","retry_after_secs":12,"assumed":false}|2|limited, wait 12 s
{"outcome":"unknown"}|3|unknown: the post may exist; do not send it again; read the thread first
{"outcome":"somethingelse"}|3|unknown: the post may exist; do not send it again; read the thread first
{"error":"the chat request failed","code":"handler_error"}|3|unknown: the post may exist; do not send it again; read the thread first
{"error":"the row now maps to another channel","code":"stale_mapping"}|2|refused stale_mapping the row now maps to another channel
EOF
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

case_a_resolve_that_maps_nothing_posts_nothing() {
    local bad="" payload want p
    while IFS='|' read -r payload want; do
        stage_reply chat.resolve "{\"v\":1,\"id\":2,\"kind\":\"res\",\"op\":\"chat.resolve\",\"payload\":$payload}"
        stage_reply chat.post "$POSTED"; start_stub_daemon
        run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- "hi"
        p="$(sent chat.post)"; stop_stub_daemon
        { [ "$CH_RC" = 2 ] && [ "$CH_OUT" = "$want" ] && [ "$p" = 0 ]; } || bad="$bad [$payload: rc=$CH_RC post=$p out=$CH_OUT]"
    done <<'EOF'
{"state":"unmapped"}|refused unmapped this row's folder maps to no channel
{"state":"invalid"}|refused invalid the chat's configuration does not map this row
{"error":"the chat is not available here","code":"unavailable"}|refused unavailable the chat is not available here
EOF
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

case_no_answer_to_the_post_is_one_request_and_exit_3() {
    stage_reply chat.resolve "$MAPPED"; start_stub_daemon
    run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- "hi"
    local p; p="$(sent chat.post)"; stop_stub_daemon
    [ "$CH_RC" = 3 ] && contains "$CH_OUT" "no answer: the post may exist; do not send it again; read the thread first" && [ "$p" = 1 ] \
        || { echo "rc=$CH_RC post_requests=$p out=$CH_OUT"; return 1; }
}

case_no_answer_to_the_resolve_posts_nothing() {
    start_stub_daemon
    run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- "hi"
    local p; p="$(sent chat.post)"; stop_stub_daemon
    [ "$CH_RC" = 2 ] && contains "$CH_OUT" "Nothing was posted" && [ "$p" = 0 ] \
        || { echo "rc=$CH_RC post_requests=$p out=$CH_OUT"; return 1; }
}

case_stdin_text_reaches_the_payload_whole() {
    # Newlines, quotes, a backslash, a dollar, a leading slash and a leading dash all survive.
    local text='/not a path ZQMARKERZ
"quoted" and '"'"'single'"'"' \n back\\slash $HOME $(date) -n
last line'
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.post "$POSTED"; start_stub_daemon
    run_chat "$text" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- --stdin --thread 1700000050.000000
    local got; got="$(posted_field .payload.text)"; stop_stub_daemon
    [ "$CH_RC" = 0 ] && [ "$got" = "$text" ] || { echo "rc=$CH_RC out=$CH_OUT"; printf 'sent:\n%s\n' "$got"; return 1; }
}

case_free_text_is_on_no_jq_command_line() {
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.post "$POSTED"; start_stub_daemon
    : > "$WORK/jq-argv.log"
    run_chat "/leading slash ZQMARKERZ" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- --stdin --thread 1700000050.000000
    local calls hits; calls="$(tr -cd '\0' < "$WORK/jq-argv.log" | wc -c)"; hits="$(grep -ac ZQMARKERZ "$WORK/jq-argv.log")"
    stop_stub_daemon
    [ "$CH_RC" = 0 ] && [ "$calls" -gt 0 ] && [ "$hits" = 0 ] || { echo "rc=$CH_RC jq_args=$calls argv_lines_with_the_text=$hits"; return 1; }
}

case_usage_errors_send_nothing_and_exit_1() {
    local bad="" args n
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.post "$POSTED"; start_stub_daemon
    for args in "" "--thread notats hi" "--thread 12 hi" "--thread 1.2.3 hi" "--stdin extra"; do
        # shellcheck disable=SC2086
        run_chat "text" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $args
        [ "$CH_RC" = 1 ] || bad="$bad [$args: rc=$CH_RC out=$CH_OUT]"
    done
    n="$(sent_any)"; stop_stub_daemon
    [ -z "$bad" ] && [ "$n" = 0 ] || { echo "$bad sent=$n"; return 1; }
}

check "outside a row the verb is refused before an endpoint is found"                       case_outside_a_row_is_refused_before_an_endpoint
check "a session with no comm handle is refused before an endpoint is found"                case_no_handle_is_refused_before_an_endpoint
check "one chat.resolve, then one chat.post of kind row with the handle and the thread"     case_one_resolve_then_one_row_post_with_handle_and_thread
check "with no --thread the post carries no thread"                                         case_without_a_thread_the_post_carries_none
check "each outcome prints its words and exits with its code"                               case_each_outcome_has_its_words_and_exit_code
check "a resolve that maps nothing, or is refused, posts nothing"                           case_a_resolve_that_maps_nothing_posts_nothing
check "no answer to the post is one request, never two, and exit 3"                         case_no_answer_to_the_post_is_one_request_and_exit_3
check "no answer to the resolve posts nothing"                                              case_no_answer_to_the_resolve_posts_nothing
check "--stdin text with newlines, quotes and a leading slash reaches the payload whole"    case_stdin_text_reaches_the_payload_whole
check "the free text is on no jq command line"                                              case_free_text_is_on_no_jq_command_line
check "a usage error sends nothing and exits 1"                                             case_usage_errors_send_nothing_and_exit_1

echo ""
echo "$PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
