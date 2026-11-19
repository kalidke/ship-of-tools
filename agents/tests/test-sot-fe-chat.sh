#!/usr/bin/env bash
# test-sot-fe-chat.sh — hermetic suite for `sot-fe chat post`, a row's reply in
# its channel. The verb posts only for the row it runs in ($SOT_WORKSPACE_ID)
# under that session's comm handle: it asks `chat.resolve` for the pair, sends
# ONE `chat.post` of kind `row` into the thread, and never a second (a post
# that may exist is not sent again, so no answer is one request, not two).
# Outside a row, or with no handle, it is refused before an endpoint is found.
# With --file it sends ONE `chat.post_file` instead, naming the file by its
# node id in the row's folder; a path outside the folder is refused before an
# endpoint is found.
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
# It runs in $CH_DIR when a case sets it.
CH_OUT=""; CH_RC=0; CH_DIR=""
run_chat() {
    local input="$1" envs=()
    shift
    while [ "$1" != "--" ]; do envs+=("$1"); shift; done; shift
    CH_OUT="$(cd "${CH_DIR:-.}" && printf '%s' "$input" | env -u SOT_WORKSPACE_ID -u SOT_WORKSPACE_ROOT -u STUB_NAME JQ_ARGV_LOG="$WORK/jq-argv.log" "${envs[@]}" "$SOT_FE" chat post "$@" --endpoint "unix:$SOCK" --timeout 3 2>&1)"
    CH_RC=$?
}
# How many requests of OP the stub has logged; and how many requests of any op (hellos included).
sent() { grep -c "\"op\":\"$1\"" "$WORK/req-$STUBN.log"; }
sent_any() { grep -c '"kind":"req"' "$WORK/req-$STUBN.log"; }
# One field of the logged chat.post, and of the logged chat.post_file.
posted_field() { jq -r "select(.op==\"chat.post\") | $1" "$WORK/req-$STUBN.log"; }
filed_field() { jq -r "select(.op==\"chat.post_file\") | $1" "$WORK/req-$STUBN.log"; }

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

# The row's folder for the file cases, with a subfolder; a figure is named by path and never read by the tool.
PROJ="$WORK/proj"
mkdir -p "$PROJ/figs"
# The folder spelled through a link above it, as macOS's /tmp spells /private/tmp.
ln -s "$PROJ" "$WORK/projlink"
SHARED='{"v":1,"id":3,"kind":"res","op":"chat.post_file","payload":{"outcome":"posted","file":"F0FILE","ts":"1700000800.000100"}}'

# The row mapped and the file post answered with the JSON object $1; the verb run with a thread, a file and a comment.
file_answered() {
    stage_reply chat.resolve "$MAPPED"
    stage_reply chat.post_file "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.post_file\",\"payload\":$1}"
    start_stub_daemon
    run_chat "" SOT_WORKSPACE_ID=$ROW SOT_WORKSPACE_ROOT="$PROJ" STUB_NAME=alpha-h -- --thread 1700000050.000000 --file "$PROJ/figs/fit.png" "the fit"
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
{"outcome":"posted","ts":"1700000000.000100","unlinked":[{"name":"\"@Zed\"","why":"no_member"},{"name":"\"@channel\"","why":"channel_wide"},{"name":"\"@cy\"","why":"later_reason"}]}|0|posted 1700000000.000100; not linked: "@Zed" (no member), "@channel" (a session never notifies a whole channel), "@cy" (not linked)
{"outcome":"posted","ts":"1700000000.000100","mentioned":["\"Ada Example\"","\"Ben Sample\""]}|0|posted 1700000000.000100; mentioned 2: "Ada Example", "Ben Sample"
{"outcome":"posted","ts":"1700000000.000100","mentioned":["\"Ada\" (\"Ann Dee\")"],"unlinked":[{"name":"\"@Zed\"","why":"no_member"}]}|0|posted 1700000000.000100; mentioned 1: "Ada" ("Ann Dee"); not linked: "@Zed" (no member)
{"outcome":"refused","code":"busy"}|2|refused busy the chat is busy; try again in a moment
{"outcome":"refused","code":"bad_request"}|2|refused bad_request the request is not valid (empty or over-long text, or a name that is not this session's)
{"outcome":"limited","retry_after_secs":12,"assumed":false}|2|limited, wait 12 s
{"outcome":"unknown"}|3|unknown: the post may exist; do not send it again; ask the person to look in the channel
{"outcome":"somethingelse"}|3|unknown: the post may exist; do not send it again; ask the person to look in the channel
{"error":"the chat request failed","code":"handler_error"}|3|unknown: the post may exist; do not send it again; ask the person to look in the channel
{"error":"the session's project now has another channel","code":"stale_mapping"}|2|refused stale_mapping the session's project now has another channel
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
{"state":"unmapped"}|refused unmapped this session's project has no channel
{"state":"unconfigured","reason":"not_connected"}|refused unconfigured no Slack workspace is connected on the computer that runs your sessions
{"state":"unconfigured","reason":"tokens_refused"}|refused unconfigured Slack refused the connected tokens
{"state":"unconfigured","reason":"wrong_app"}|refused unconfigured the connected Slack tokens belong to two different apps
{"state":"unconfigured","reason":"start_failed"}|refused unconfigured the chat could not start
{"state":"unconfigured"}|refused unconfigured the chat is not set up on the computer that runs your sessions
{"state":"invalid"}|refused invalid the chat's configuration does not map this session's project
{"error":"the chat is not available here","code":"unavailable"}|refused unavailable the chat is not available here
EOF
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

case_no_answer_to_the_post_is_one_request_and_exit_3() {
    stage_reply chat.resolve "$MAPPED"; start_stub_daemon
    run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- "hi"
    local p; p="$(sent chat.post)"; stop_stub_daemon
    [ "$CH_RC" = 3 ] && contains "$CH_OUT" "no answer: the post may exist; do not send it again; ask the person to look in the channel" && [ "$p" = 1 ] \
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

case_a_file_post_is_refused_before_an_endpoint_outside_a_row_its_folder_or_a_handle() {
    local bad="" envs path n
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.post_file "$SHARED"; start_stub_daemon
    while IFS='|' read -r envs path; do
        # shellcheck disable=SC2086
        run_chat "" $envs -- --file "$path" "a comment"
        [ "$CH_RC" = 2 ] && contains "$CH_OUT" "Nothing was sent" || bad="$bad [$envs $path: rc=$CH_RC out=$CH_OUT]"
    done <<EOF
STUB_NAME=alpha-h SOT_WORKSPACE_ROOT=$PROJ|$PROJ/figs/fit.png
SOT_WORKSPACE_ID=$ROW SOT_WORKSPACE_ROOT=$PROJ|$PROJ/figs/fit.png
SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h|$PROJ/figs/fit.png
SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h SOT_WORKSPACE_ROOT=$PROJ|$WORK/elsewhere.png
SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h SOT_WORKSPACE_ROOT=$PROJ|${PROJ}x/fit.png
SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h SOT_WORKSPACE_ROOT=$PROJ|$PROJ/figs/../../elsewhere.png
SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h SOT_WORKSPACE_ROOT=$PROJ|$PROJ/
SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h SOT_WORKSPACE_ROOT=$PROJ|fit.png
EOF
    n="$(sent_any)"; stop_stub_daemon
    [ -z "$bad" ] && [ "$n" = 0 ] || { echo "$bad sent=$n"; return 1; }
}

case_a_file_post_is_one_resolve_then_one_post_file_with_its_node_thread_and_comment() {
    local bad="" dir path want bare r p t k n i c x h args wt wc
    while IFS='|' read -r dir path want bare; do
        stage_reply chat.resolve "$MAPPED"; stage_reply chat.post_file "$SHARED"; start_stub_daemon
        # A bare row sends neither a thread nor a comment, and the request carries neither.
        if [ -n "$bare" ]; then args=(--file "$path"); wt=null; wc=null; else args=(--thread 1700000050.000000 --file "$path" "the fit"); wt=1700000050.000000; wc="the fit"; fi
        CH_DIR="$dir" run_chat "" SOT_WORKSPACE_ID=$ROW SOT_WORKSPACE_ROOT="$PROJ/" STUB_NAME=alpha-h -- "${args[@]}"
        r="$(sent chat.resolve)"; p="$(sent chat.post_file)"; x="$(sent chat.post)"
        k="$(filed_field .payload.kind)"; n="$(filed_field .payload.name)"; t="$(filed_field .payload.thread)"
        i="$(filed_field .payload.node_id)"; c="$(filed_field .payload.comment)"; h="$(filed_field .payload.target.channel)"
        stop_stub_daemon
        { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "posted 1700000800.000100" ] && [ "$r" = 1 ] && [ "$p" = 1 ] && [ "$x" = 0 ] \
            && [ "$k" = row ] && [ "$n" = alpha-h ] && [ "$t" = "$wt" ] && [ "$i" = "$want" ] && [ "$c" = "$wc" ] && [ "$h" = C0AAAA ]; } \
            || bad="$bad [$dir $path $bare: rc=$CH_RC out=$CH_OUT resolve=$r post_file=$p post=$x kind=$k name=$n thread=$t node=$i comment=$c channel=$h]"
    done <<EOF
$WORK|$PROJ/figs/fit.png|files:figs/fit.png|
$PROJ/figs|fit.png|files:figs/fit.png|
$PROJ|./figs/a b.png|files:./figs/a b.png|
$WORK|$WORK/projlink/figs/fit.png|files:figs/fit.png|
$WORK/projlink/figs|fit.png|files:figs/fit.png|
$WORK|$PROJ/figs/fit.png|files:figs/fit.png|bare
EOF
    CH_DIR=""
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

case_each_file_outcome_has_its_words_and_exit_code() {
    local bad="" want_rc want_text payload
    while IFS='|' read -r payload want_rc want_text; do
        file_answered "$payload"
        stop_stub_daemon
        { [ "$CH_RC" = "$want_rc" ] && [ "$CH_OUT" = "$want_text" ]; } \
            || bad="$bad [$payload: rc=$CH_RC out=$CH_OUT]"
    done <<'EOF'
{"outcome":"posted","file":"F0FILE","ts":"1700000800.000100"}|0|posted 1700000800.000100
{"outcome":"posted","file":"F0FILE"}|0|posted file F0FILE
{"outcome":"posted","file":"F0FILE","unlinked":[{"name":"\"@Ada\"","why":"members_unread"}]}|0|posted file F0FILE; not linked: "@Ada" (the member list could not be read)
{"outcome":"posted","file":"F0FILE","unlinked":[{"name":"\"@Ada\"","why":"no_member"},{"name":"","why":"more"}]}|0|posted file F0FILE; not linked: "@Ada" (no member), and more
{"outcome":"posted","file":"F0FILE","mentioned":["\"Ada Example\"",""],"unlinked":[{"name":"\"@Ben\"","why":"several_members","candidates":["\"Ben\" (\"Ben Smith\")","\"Ben Sample\"",""]}]}|0|posted file F0FILE; mentioned 1 and more: "Ada Example", and more; not linked: "@Ben" (several members, 2 and more: "Ben" ("Ben Smith"), "Ben Sample", and more)
{"outcome":"refused","code":"busy"}|2|refused busy the chat is busy; try again in a moment
{"outcome":"refused","code":"failed"}|2|refused failed the chat request failed
{"error":"the file is not a readable file in the project's folder","code":"not_a_file"}|2|refused not_a_file the file is not a readable file in the project's folder
{"error":"the file is larger than the chat carries","code":"too_large"}|2|refused too_large the file is larger than the chat carries
{"outcome":"limited","retry_after_secs":12,"assumed":false}|2|limited, wait 12 s
{"outcome":"unknown"}|3|unknown: the post may exist; do not send it again; ask the person to look in the channel
{"outcome":"somethingelse"}|3|unknown: the post may exist; do not send it again; ask the person to look in the channel
{"error":"the chat request failed","code":"handler_error"}|3|unknown: the post may exist; do not send it again; ask the person to look in the channel
EOF
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

case_no_answer_to_the_file_post_is_one_request_and_exit_3() {
    stage_reply chat.resolve "$MAPPED"; start_stub_daemon
    run_chat "" SOT_WORKSPACE_ID=$ROW SOT_WORKSPACE_ROOT="$PROJ" STUB_NAME=alpha-h -- --file "$PROJ/figs/fit.png" "the fit"
    local p; p="$(sent chat.post_file)"; stop_stub_daemon
    [ "$CH_RC" = 3 ] && contains "$CH_OUT" "no answer: the post may exist; do not send it again; ask the person to look in the channel" && [ "$p" = 1 ] \
        || { echo "rc=$CH_RC post_file_requests=$p out=$CH_OUT"; return 1; }
}

case_a_file_usage_error_sends_nothing_and_exits_1() {
    local bad="" n
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.post_file "$SHARED"; start_stub_daemon
    for args in "--file" "--file $PROJ/figs/fit.png one two" "--file $PROJ/figs/fit.png --stdin extra"; do
        # shellcheck disable=SC2086
        run_chat "text" SOT_WORKSPACE_ID=$ROW SOT_WORKSPACE_ROOT="$PROJ" STUB_NAME=alpha-h -- $args
        [ "$CH_RC" = 1 ] || bad="$bad [$args: rc=$CH_RC out=$CH_OUT]"
    done
    run_chat "" SOT_WORKSPACE_ID=$ROW SOT_WORKSPACE_ROOT="$PROJ" STUB_NAME=alpha-h -- --file ""
    [ "$CH_RC" = 1 ] || bad="$bad [empty --file: rc=$CH_RC out=$CH_OUT]"
    n="$(sent_any)"; stop_stub_daemon
    [ -z "$bad" ] && [ "$n" = 0 ] || { echo "$bad sent=$n"; return 1; }
}

# The daemon's report cannot be forged by a member's names (chat guarantee 35): the reviewer's pair, whose real name
# holds `x), Ada (Ada Lovelace`, prints as one entry, and the printed line splits back on its quotes into exactly the
# entries sent, a handle after one of them included.
case_the_mentioned_report_splits_back_into_the_entries_sent() {
    local forged='"Ada please" ("x), Ada (Ada Lovelace")' twin='"Ada" ("Ada Example") @ada.copy' payload section got
    payload="$(jq -cn --arg a "$forged" --arg b "$twin" '{outcome:"posted", ts:"1700000000.000100", mentioned:[$a, $b]}')"
    answered "$payload"
    stop_stub_daemon
    section="${CH_OUT#posted 1700000000.000100; mentioned 2: }"
    got="$(printf '%s' "$section" | grep -oE '"[^"]*"( \("[^"]*"\))?( @[A-Za-z0-9._-]+)?' | paste -sd '|' -)"
    [ "$CH_RC" = 0 ] && [ "$section" = "$forged, $twin" ] && [ "$got" = "$forged|$twin" ] \
        || { echo "rc=$CH_RC out=$CH_OUT got=$got"; return 1; }
}

# Each wait is past the sum of the daemon's own bounds for that post, read from sot-chat's constants, by at least 5 s,
# so a made post or a refusal always arrives before the verb gives up and says it may exist.
case_each_wait_is_past_the_daemons_own_bounds() {
    local api="$SCRIPT_DIR/../../rust/chat/src/api.rs" chat="$SCRIPT_DIR/../sot-fe/sot-fe-chat.sh" read post transfer names text file
    secs() { sed -n "s/^pub const $1: Duration = Duration::from_secs(\([0-9]*\));.*/\1/p" "$api"; }
    read="$(secs READ_DEADLINE)"; post="$(secs POST_DEADLINE)"; transfer="$(secs TRANSFER_DEADLINE)"; names="$(secs NAMES_DEADLINE)"
    text="$(sed -n '/^send_chat_post()/,/^}/s/.*TIMEOUT:-\([0-9]*\)}.*/\1/p' "$chat")"
    file="$(sed -n '/^send_chat_post_file()/,/^}/s/.*TIMEOUT:-\([0-9]*\)}.*/\1/p' "$chat")"
    [ -n "$read" ] && [ -n "$post" ] && [ -n "$transfer" ] && [ -n "$names" ] && [ -n "$text" ] && [ -n "$file" ] \
        && [ "$text" -ge $((names + post + 5)) ] && [ "$file" -ge $((names + read + transfer + post + 5)) ] \
        || { echo "read=$read post=$post transfer=$transfer names=$names text=$text file=$file"; return 1; }
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
check "a file outside the row's folder, or with no row, root or handle, sends nothing"     case_a_file_post_is_refused_before_an_endpoint_outside_a_row_its_folder_or_a_handle
check "a file post is one chat.resolve, then one chat.post_file with node, thread, comment, or neither" case_a_file_post_is_one_resolve_then_one_post_file_with_its_node_thread_and_comment
check "each file outcome prints its words and exits with its code"                         case_each_file_outcome_has_its_words_and_exit_code
check "no answer to the file post is one request, never two, and exit 3"                    case_no_answer_to_the_file_post_is_one_request_and_exit_3
check "a file usage error sends nothing and exits 1"                                       case_a_file_usage_error_sends_nothing_and_exits_1
check "the mentioned report splits back into exactly the entries sent"                      case_the_mentioned_report_splits_back_into_the_entries_sent
check "each wait is past the daemon's own bounds for that post, with a margin"             case_each_wait_is_past_the_daemons_own_bounds

echo ""
echo "$PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
