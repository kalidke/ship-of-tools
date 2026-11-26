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
# `sot-fe chat delete <ts> [--thread <ts>]` is one `chat.resolve`, then ONE `chat.delete_post` for that ts (and thread) with
# the row's handle and the resolved channel, never a second; it prints a line for each file the post's delete deleted or
# kept; its exit codes are a post's (0 deleted or gone, 1 usage, 2 refused, 3 unknown).
# `sot-fe chat unpair <session>` is ONE `chat.row_pair` with the row, the handle and a null channel and no resolve, never a
# second; its exit codes are 0 unpaired or unchanged, 1 usage, 2 refused (nothing changed), 3 no answer (it may have been made).
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
    { exec 3>&-; } 2>/dev/null || true
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

    await test -S "$SOCK" || echo "stub daemon socket never appeared: $SOCK" >&2
}

stop_stub_daemon() {
    end_group "$STUB_WATCHER_PID"
    end_group "$STUB_NC_PID"
    { exec 3>&-; } 2>/dev/null || true
    STUB_NC_PID=""
    STUB_WATCHER_PID=""
    STAGED_REPLY=()
}

# --- helpers ---------------------------------------------------------------

# run_chat STDIN ENVSPEC... -- ARGS...: run `sot-fe chat ${CH_VERB:-post} ARGS` against the stub with only the given variables
# and STDIN as its standard input.
# It runs in $CH_DIR when a case sets it.
CH_OUT=""; CH_RC=0; CH_DIR=""
run_chat() {
    local input="$1" envs=()
    shift
    while [ "$1" != "--" ]; do envs+=("$1"); shift; done; shift
    CH_OUT="$(cd "${CH_DIR:-.}" && printf '%s' "$input" | env -u SOT_WORKSPACE_ID -u SOT_WORKSPACE_ROOT -u STUB_NAME JQ_ARGV_LOG="$WORK/jq-argv.log" "${envs[@]}" "$SOT_FE" chat "${CH_VERB:-post}" "$@" --endpoint "unix:$SOCK" --timeout 3 2>&1)"
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
{"outcome":"unknown"}|3|unknown: the post may exist; do not send it again; to see whether it went out, run sot-fe chat read --thread 1700000050.000000
{"outcome":"somethingelse"}|3|unknown: the post may exist; do not send it again; to see whether it went out, run sot-fe chat read --thread 1700000050.000000
{"error":"the chat request failed","code":"handler_error"}|3|unknown: the post may exist; do not send it again; to see whether it went out, run sot-fe chat read --thread 1700000050.000000
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
    [ "$CH_RC" = 3 ] && contains "$CH_OUT" "no answer: the post may exist; do not send it again; to see whether it went out, run sot-fe chat read" && ! contains "$CH_OUT" "--thread" && [ "$p" = 1 ] \
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
        want_text="$(printf '%b' "$want_text")"
        { [ "$CH_RC" = "$want_rc" ] && [ "$CH_OUT" = "$want_text" ]; } \
            || bad="$bad [$payload: rc=$CH_RC out=$CH_OUT]"
    done <<'EOF'
{"outcome":"posted","file":"F0FILE","ts":"1700000800.000100"}|0|posted 1700000800.000100
{"outcome":"posted","file":"F0FILE"}|0|posted file F0FILE\nits message's time is not known yet: run sot-fe chat read --thread 1700000050.000000, whose line for this post shows files=F0FILE
{"outcome":"posted","file":"F0FILE","unlinked":[{"name":"\"@Ada\"","why":"members_unread"}]}|0|posted file F0FILE; not linked: "@Ada" (the member list could not be read)\nits message's time is not known yet: run sot-fe chat read --thread 1700000050.000000, whose line for this post shows files=F0FILE
{"outcome":"posted","file":"F0FILE","unlinked":[{"name":"\"@Ada\"","why":"no_member"},{"name":"","why":"more"}]}|0|posted file F0FILE; not linked: "@Ada" (no member), and more\nits message's time is not known yet: run sot-fe chat read --thread 1700000050.000000, whose line for this post shows files=F0FILE
{"outcome":"posted","file":"F0FILE","mentioned":["\"Ada Example\"",""],"unlinked":[{"name":"\"@Ben\"","why":"several_members","candidates":["\"Ben\" (\"Ben Smith\")","\"Ben Sample\"",""]}]}|0|posted file F0FILE; mentioned 1 and more: "Ada Example", and more; not linked: "@Ben" (several members, 2 and more: "Ben" ("Ben Smith"), "Ben Sample", and more)\nits message's time is not known yet: run sot-fe chat read --thread 1700000050.000000, whose line for this post shows files=F0FILE
{"outcome":"refused","code":"busy"}|2|refused busy the chat is busy; try again in a moment
{"outcome":"refused","code":"failed"}|2|refused failed the chat request failed
{"error":"the file is not a readable file in the project's folder","code":"not_a_file"}|2|refused not_a_file the file is not a readable file in the project's folder
{"error":"the file is larger than the chat carries","code":"too_large"}|2|refused too_large the file is larger than the chat carries
{"outcome":"limited","retry_after_secs":12,"assumed":false}|2|limited, wait 12 s
{"outcome":"unknown"}|3|unknown: the post may exist; do not send it again; to see whether it went out, run sot-fe chat read --thread 1700000050.000000
{"outcome":"somethingelse"}|3|unknown: the post may exist; do not send it again; to see whether it went out, run sot-fe chat read --thread 1700000050.000000
{"error":"the chat request failed","code":"handler_error"}|3|unknown: the post may exist; do not send it again; to see whether it went out, run sot-fe chat read --thread 1700000050.000000
EOF
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

# A file share whose reply names no message time prints its id, and a second line that says how to find the time.
case_a_file_post_with_no_time_says_how_to_find_it() {
    local bad="" thread want
    for thread in 1700000050.000000 ""; do
        stage_reply chat.resolve "$MAPPED"
        stage_reply chat.post_file '{"v":1,"id":3,"kind":"res","op":"chat.post_file","payload":{"outcome":"posted","file":"F0FILE"}}'
        start_stub_daemon
        run_chat "" SOT_WORKSPACE_ID=$ROW SOT_WORKSPACE_ROOT="$PROJ" STUB_NAME=alpha-h -- ${thread:+--thread $thread} --file "$PROJ/figs/fit.png" "the fit"
        stop_stub_daemon
        want="$(printf '%s\n%s' "posted file F0FILE" "its message's time is not known yet: run sot-fe chat read${thread:+ --thread $thread}, whose line for this post shows files=F0FILE")"
        { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "$want" ]; } || bad="$bad [thread '$thread': rc=$CH_RC out=$CH_OUT]"
    done
    # A reply that names the time is the one line, and a text post is never given the second line.
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.post_file "$SHARED"; start_stub_daemon
    run_chat "" SOT_WORKSPACE_ID=$ROW SOT_WORKSPACE_ROOT="$PROJ" STUB_NAME=alpha-h -- --file "$PROJ/figs/fit.png" "the fit"
    stop_stub_daemon
    { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "posted 1700000800.000100" ]; } || bad="$bad [named: rc=$CH_RC out=$CH_OUT]"
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

case_no_answer_to_the_file_post_is_one_request_and_exit_3() {
    stage_reply chat.resolve "$MAPPED"; start_stub_daemon
    run_chat "" SOT_WORKSPACE_ID=$ROW SOT_WORKSPACE_ROOT="$PROJ" STUB_NAME=alpha-h -- --file "$PROJ/figs/fit.png" "the fit"
    local p; p="$(sent chat.post_file)"; stop_stub_daemon
    [ "$CH_RC" = 3 ] && contains "$CH_OUT" "no answer: the post may exist; do not send it again; to see whether it went out, run sot-fe chat read" && ! contains "$CH_OUT" "--thread" && [ "$p" = 1 ] \
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

# `sot-fe chat post`'s copy of the report's words is wire's `told_words`: every case of
# rust/protocol/src/ops/told_words_cases.tsv (a reply's payload, a tab, the line), which the protocol test
# `sot_fe_and_the_wire_print_the_same_report` asserts on the wire's side, is printed here exactly.
case_sot_fe_prints_the_wire_report_for_every_case() {
    local cases="$SCRIPT_DIR/../../rust/protocol/src/ops/told_words_cases.tsv" bad="" n=0 payload line tab got
    tab="$(printf '\t')"
    [ -f "$cases" ] || { echo "no $cases"; return 1; }
    while IFS= read -r row; do
        case "$row" in '#'*|'') continue ;; esac
        payload="${row%%"$tab"*}"; line="${row#*"$tab"}"
        if printf '%s' "$payload" | jq -e 'has("file")' >/dev/null; then file_answered "$payload"; else answered "$payload"; fi
        stop_stub_daemon
        n=$((n + 1))
        # A file share that names no time prints the report, then a second line of its own (a case of its own pins it).
        got="$CH_OUT"
        if printf '%s' "$payload" | jq -e 'has("file") and (has("ts") | not)' >/dev/null; then
            contains "$CH_OUT" $'\nits message\'s time is not known yet: ' || bad="$bad [no second line: $CH_OUT]"
            got="${CH_OUT%%$'\n'*}"
        fi
        { [ "$CH_RC" = 0 ] && [ "$got" = "$line" ]; } || bad="$bad [want: $line | got rc=$CH_RC: $CH_OUT]"
    done < "$cases"
    [ "$n" -ge 10 ] && [ -z "$bad" ] || { echo "cases=$n$bad"; return 1; }
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
        && [ "$text" -ge $((names + post + 5)) ] && [ "$file" -ge $((names + 2 * read + transfer + post + 5)) ] \
        || { echo "read=$read post=$post transfer=$transfer names=$names text=$text file=$file"; return 1; }
}

# --- chat delete ---------------------------------------------------------

TS=1700000000.000100
THR=1700000000.000050
# The row mapped and the delete answered with the JSON object $1; the verb run for $TS (and the thread $2, when given).
delete_answered() {
    stage_reply chat.resolve "$MAPPED"
    stage_reply chat.delete_post "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.delete_post\",\"payload\":$1}"
    start_stub_daemon
    CH_VERB=delete run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $TS ${2:+--thread $2}
}
deleted_field() { jq -r "select(.op==\"chat.delete_post\") | $1" "$WORK/req-$STUBN.log"; }

case_a_delete_outside_a_row_or_with_no_handle_sends_nothing() {
    local bad="" envs n
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.delete_post '{}'; start_stub_daemon
    for envs in "STUB_NAME=alpha-h" "SOT_WORKSPACE_ID=$ROW"; do
        # shellcheck disable=SC2086
        CH_VERB=delete run_chat "" $envs -- $TS
        [ "$CH_RC" = 2 ] && contains "$CH_OUT" "Nothing was sent" || bad="$bad [$envs: rc=$CH_RC out=$CH_OUT]"
    done
    n="$(sent_any)"; stop_stub_daemon
    [ -z "$bad" ] && [ "$n" = 0 ] || { echo "$bad sent=$n"; return 1; }
}

# One chat.resolve, then ONE chat.delete_post for that ts with the row's handle and the resolved channel, no thread when none
# was given, and never the older op; each file the reply names is a line, an id with a control byte in it with spaces.
case_delete_sends_one_delete_post_and_prints_each_file() {
    delete_answered '{"outcome":"deleted","files":{"deleted":["F0AAAA","F0BB\u0007BB"],"kept":[{"file":"F0CCCC","why":"not_the_apps"},{"file":"F0DDDD","why":"shared_elsewhere"},{"file":"F0EEEE","why":"from_the_future"}]}}'
    local r d old first rw w c h n t k keys want
    r="$(sent chat.resolve)"; d="$(sent chat.delete_post)"; old="$(sent chat.delete)"
    first="$(jq -r 'select(.op=="chat.resolve" or .op=="chat.delete_post") | .op' "$WORK/req-$STUBN.log" | head -n 1)"
    rw="$(jq -r 'select(.op=="chat.resolve") | .payload.row' "$WORK/req-$STUBN.log")"
    h="$(deleted_field .payload.target.row)"; w="$(deleted_field .payload.target.workspace)"; c="$(deleted_field .payload.target.channel)"
    n="$(deleted_field .payload.name)"; t="$(deleted_field .payload.ts)"; k="$(deleted_field '.payload | has("kind")')"
    keys="$(deleted_field '.payload | keys | join(",")')"
    stop_stub_daemon
    want="$(printf '%s\n' "deleted $TS" "file F0AAAA deleted" "file F0BB BB deleted" \
        "file F0CCCC kept: it is not this project's Slack app's file (a person's or another app's)" \
        "file F0DDDD kept: it is also shared somewhere else in Slack" "file F0EEEE kept")"
    [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "$want" ] && [ "$r" = 1 ] && [ "$d" = 1 ] && [ "$old" = 0 ] && [ "$first" = chat.resolve ] && [ "$rw" = "$ROW" ] \
        && [ "$h" = "$ROW" ] && [ "$w" = T0AAAA ] && [ "$c" = C0AAAA ] && [ "$n" = alpha-h ] && [ "$t" = "$TS" ] && [ "$k" = false ] \
        && [ "$keys" = "name,target,ts" ] \
        || { echo "rc=$CH_RC out=$CH_OUT resolve=$r delete_post=$d old=$old first=$first row=$rw target_row=$h ws=$w ch=$c name=$n ts=$t kind=$k keys=$keys"; return 1; }
}

case_each_delete_post_answer_has_its_words_and_exit_code() {
    local bad="" want_rc want_text payload
    while IFS='|' read -r payload want_rc want_text; do
        delete_answered "$payload"
        stop_stub_daemon
        want_text="$(printf '%b' "$want_text")"
        { [ "$CH_RC" = "$want_rc" ] && [ "$CH_OUT" = "$want_text" ]; } \
            || bad="$bad [$payload: rc=$CH_RC out=$CH_OUT]"
    done <<'EOF'
{"outcome":"deleted"}|0|deleted 1700000000.000100
{"outcome":"deleted","files":{"deleted":["F0AAAA"]}}|0|deleted 1700000000.000100\nfile F0AAAA deleted
{"outcome":"gone"}|0|gone 1700000000.000100: no message with that time at the top of the channel (deleted already, or never posted); a reply was not looked for: run again with --thread <the time of its thread's first message>
{"outcome":"gone","files":{"kept":[{"file":"F0CCCC","why":"not_the_apps"}]}}|0|gone 1700000000.000100: no message with that time at the top of the channel (deleted already, or never posted); a reply was not looked for: run again with --thread <the time of its thread's first message>\nfile F0CCCC kept: it is not this project's Slack app's file (a person's or another app's)
{"outcome":"refused","code":"not_own_message"}|2|refused not_own_message that message is not one this project's Slack app posted, or Slack would not let the app delete it (a person's or another app's message stays)
{"outcome":"refused","code":"unclear_file","files":{"deleted":["F0AAAA"]}}|2|refused unclear_file a file of that post could not be checked as this project's Slack app's own and shared only in that post, so nothing was deleted (a person can delete the post in Slack)\nfile F0AAAA deleted
{"outcome":"refused","code":"unavailable"}|2|refused unavailable the chat is not available here; nothing was sent
{"outcome":"refused","code":"busy","files":{"deleted":["F0AAAA"]}}|2|refused busy Slack answered too slowly to finish in time, or the chat is busy; the post is not deleted yet; run the same command again\nfile F0AAAA deleted
{"outcome":"refused","code":"bad_request"}|2|refused bad_request the request could not be sent (this session's handle is not its session's, or a time is not a message's time); nothing was deleted
{"error":"the request is not valid","code":"bad_request"}|2|refused bad_request the request is not valid
{"outcome":"refused","code":"failed"}|2|refused failed the chat request failed; the post is not deleted
{"outcome":"refused","code":"stale_row"}|2|refused stale_row the session is gone or was replaced
{"outcome":"refused","code":"stale_mapping","files":{"deleted":["F0AAAA"]}}|2|refused stale_mapping the session's project now has another channel\nfile F0AAAA deleted
{"outcome":"refused","code":"unmapped"}|2|refused unmapped the session's project has no channel
{"outcome":"refused","code":"not_in_channel"}|2|refused not_in_channel your Slack app is not in that channel
{"outcome":"limited","retry_after_secs":12,"assumed":false}|2|limited, wait 12 s: Slack asked to wait; the post is not deleted yet
{"outcome":"limited","retry_after_secs":30,"assumed":true,"files":{"deleted":["F0AAAA"]}}|2|limited, try again later: Slack asked for a pause; the post is not deleted yet\nfile F0AAAA deleted
{"outcome":"unknown"}|3|unknown: the post may be partly deleted; run the same command again, which finishes it or says it is gone
{"outcome":"somethingelse"}|3|unknown: the post may be partly deleted; run the same command again, which finishes it or says it is gone
{"error":"the chat request failed","code":"handler_error"}|3|unknown: the post may be partly deleted; run the same command again, which finishes it or says it is gone
{"error":"the session's project now has another channel","code":"stale_mapping"}|2|refused stale_mapping the session's project now has another channel
EOF
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

# A reply is deleted with the thread it is in: the thread rides in the request, the words of `gone` name it, and a thread
# that is no ts is a usage error before anything is sent.
case_delete_takes_a_thread_and_refuses_one_that_is_no_ts() {
    local bad="" t keys args n
    delete_answered '{"outcome":"gone"}' $THR
    t="$(deleted_field .payload.thread)"; keys="$(deleted_field '.payload | keys | join(",")')"
    stop_stub_daemon
    [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "gone $TS: thread $THR lists no message with that time (deleted already, or never posted)" ] \
        && [ "$t" = "$THR" ] && [ "$keys" = "name,target,thread,ts" ] || bad="$bad [gone: rc=$CH_RC out=$CH_OUT thread=$t keys=$keys]"
    delete_answered '{"outcome":"deleted","files":{"deleted":["F0AAAA"]}}' $THR
    t="$(deleted_field .payload.thread)"; stop_stub_daemon
    [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "$(printf '%s\n' "deleted $TS" "file F0AAAA deleted")" ] && [ "$t" = "$THR" ] || bad="$bad [deleted: rc=$CH_RC out=$CH_OUT thread=$t]"
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.delete_post '{}'; start_stub_daemon
    for args in "$TS --thread notats" "$TS --thread 17.5" "$TS --thread 1700000000.00005" "$TS --thread 1700000000.0000500" "$TS --thread"; do
        # shellcheck disable=SC2086
        CH_VERB=delete run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $args
        [ "$CH_RC" = 1 ] || bad="$bad [$args: rc=$CH_RC out=$CH_OUT]"
    done
    n="$(sent_any)"; stop_stub_daemon
    [ -z "$bad" ] && [ "$n" = 0 ] || { echo "$bad sent=$n"; return 1; }
}

# A daemon that does not know the op deletes nothing, and the words say why; the older op is never tried.
case_an_older_daemon_cannot_delete_a_post_with_its_files() {
    delete_answered '{"error":"unknown op: chat.delete_post"}'
    local old d; old="$(sent chat.delete)"; d="$(sent chat.delete_post)"; stop_stub_daemon
    [ "$CH_RC" = 2 ] && [ "$CH_OUT" = "refused unsupported: the computer that runs your sessions runs an older Ship of Tools, which cannot delete a post with its files. Nothing was deleted." ] \
        && [ "$d" = 1 ] && [ "$old" = 0 ] || { echo "rc=$CH_RC out=$CH_OUT delete_post=$d old=$old"; return 1; }
}

case_a_delete_that_maps_nothing_deletes_nothing() {
    local bad="" payload want d
    while IFS='|' read -r payload want; do
        stage_reply chat.resolve "{\"v\":1,\"id\":2,\"kind\":\"res\",\"op\":\"chat.resolve\",\"payload\":$payload}"
        stage_reply chat.delete_post '{}'; start_stub_daemon
        CH_VERB=delete run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $TS
        d="$(sent chat.delete_post)"; stop_stub_daemon
        { [ "$CH_RC" = 2 ] && [ "$CH_OUT" = "$want" ] && [ "$d" = 0 ]; } || bad="$bad [$payload: rc=$CH_RC delete=$d out=$CH_OUT]"
    done <<'EOF'
{"state":"unmapped"}|refused unmapped this session's project has no channel
{"state":"unconfigured","reason":"not_connected"}|refused unconfigured no Slack workspace is connected on the computer that runs your sessions
{"error":"the chat is not available here","code":"unavailable"}|refused unavailable the chat is not available here
EOF
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

case_no_answer_to_the_delete_is_one_request_and_exit_3() {
    stage_reply chat.resolve "$MAPPED"; start_stub_daemon
    CH_VERB=delete run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $TS
    local d; d="$(sent chat.delete_post)"; stop_stub_daemon
    [ "$CH_RC" = 3 ] && contains "$CH_OUT" "no answer: the post may be partly deleted; run the same command again, which finishes it or says it is gone" && [ "$d" = 1 ] \
        || { echo "rc=$CH_RC delete_requests=$d out=$CH_OUT"; return 1; }
}

case_no_answer_to_the_resolve_deletes_nothing() {
    start_stub_daemon
    CH_VERB=delete run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $TS
    local d; d="$(sent chat.delete_post)"; stop_stub_daemon
    [ "$CH_RC" = 2 ] && contains "$CH_OUT" "Nothing was deleted" && ! contains "$CH_OUT" "chat.resolve" && [ "$d" = 0 ] \
        || { echo "rc=$CH_RC delete_requests=$d out=$CH_OUT"; return 1; }
}

case_a_delete_usage_error_sends_nothing_and_exits_1() {
    local bad="" args n
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.delete_post '{}'; start_stub_daemon
    for args in "" "notats" "17.5" "1700000000.00010" "1700000000.0001000" "1700000000.000100 1700000000.000101" "$TS --stdin" "$TS --file x.png" "--thread $TS"; do
        # shellcheck disable=SC2086
        CH_VERB=delete run_chat "text" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $args
        [ "$CH_RC" = 1 ] || bad="$bad [$args: rc=$CH_RC out=$CH_OUT]"
    done
    n="$(sent_any)"; stop_stub_daemon
    [ -z "$bad" ] && [ "$n" = 0 ] || { echo "$bad sent=$n"; return 1; }
}

# The delete's wait is past the daemon's own bound for the whole delete, read from sot-chat's constant, by at least 5 s:
# DELETE_POST_DEADLINE.
case_the_delete_wait_is_past_the_daemons_own_bounds() {
    local del="$SCRIPT_DIR/../../rust/chat/src/delete.rs" chat="$SCRIPT_DIR/../sot-fe/sot-fe-chat.sh" bound wait
    bound="$(sed -n "s/^pub const DELETE_POST_DEADLINE: Duration = Duration::from_secs(\([0-9]*\));.*/\1/p" "$del")"
    wait="$(sed -n '/^send_chat_delete()/,/^}/s/.*TIMEOUT:-\([0-9]*\)}.*/\1/p' "$chat")"
    [ -n "$bound" ] && [ -n "$wait" ] && [ "$wait" -ge $((bound + 5)) ] \
        || { echo "bound=$bound wait=$wait"; return 1; }
}

# --- chat unpair -----------------------------------------------------------

# The row_pair answered with the JSON object $1; `unpair .` run as the row, the handle alpha-h.
PAIRING='{"workspace":"T0AAAA","channel":"C0AAAA","name":"general","workspace_tag":"lab"}'
unpair_answered() {
    stage_reply chat.row_pair "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.row_pair\",\"payload\":$1}"
    start_stub_daemon
    CH_VERB=unpair run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- "${UNPAIR_SESSION:-.}"
}
paired_field() { jq -r "select(.op==\"chat.row_pair\") | $1" "$WORK/req-$STUBN.log"; }

case_unpair_sends_one_row_pair_with_the_handle_and_prints_what_it_removed() {
    local bad="" session want_row r any by_row by_name row ch keys
    for session in . alpha ws-other-3c4d; do
        UNPAIR_SESSION="$session" unpair_answered "{\"outcome\":\"unpaired\",\"folder\":\"/p/alpha\",\"pairing\":$PAIRING}"
        r="$(sent chat.row_pair)"; any="$(sent chat.resolve)"
        by_row="$(paired_field .payload.by.row)"; by_name="$(paired_field .payload.by.name)"; row="$(paired_field .payload.row)"
        ch="$(paired_field '.payload.channel')"; keys="$(paired_field '.payload | keys | join(",")')"
        stop_stub_daemon
        want_row="$session"; [ "$session" != . ] || want_row="$ROW"
        { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "unpaired /p/alpha from #general (C0AAAA) in lab (T0AAAA)" ] && [ "$r" = 1 ] && [ "$any" = 0 ] \
            && [ "$by_row" = "$ROW" ] && [ "$by_name" = alpha-h ] && [ "$row" = "$want_row" ] && [ "$ch" = null ] && [ "$keys" = "by,channel,row" ]; } \
            || bad="$bad [$session: rc=$CH_RC out=$CH_OUT row_pair=$r resolves=$any by=$by_row/$by_name row=$row channel=$ch keys=$keys]"
    done
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

# An unpairing of a channel that delivered says its posts no longer reach the session as mail, in this project's or that one's
# words as a pairing's are; one that delivered nothing says nothing of mail.
case_unpairing_a_delivering_channel_says_its_posts_stop_reaching_the_session() {
    local bad="" session delivers want whose
    for session in . "$ROW" alpha ws-other-3c4d; do
        for delivers in true false none; do
            case "$delivers" in none) pairing="$PAIRING" ;; *) pairing="${PAIRING%\}},\"delivers\":$delivers}" ;; esac
            UNPAIR_SESSION="$session" unpair_answered "{\"outcome\":\"unpaired\",\"folder\":\"/p/alpha\",\"pairing\":$pairing}"
            stop_stub_daemon
            whose=that; { [ "$session" = . ] || [ "$session" = "$ROW" ]; } && whose=this
            want="unpaired /p/alpha from #general (C0AAAA) in lab (T0AAAA)"
            [ "$delivers" != true ] || want="$want; new posts there no longer reach $whose project's session as mail"
            { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "$want" ]; } || bad="$bad [$session $delivers: rc=$CH_RC out=$CH_OUT]"
        done
    done
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

# Each answer, the words and the code. `<session>` is as typed, and `this session's project` for `.`.
case_each_unpair_answer_has_its_words_and_exit_code() {
    local bad="" want_rc want_text payload session
    while IFS='|' read -r session payload want_rc want_text; do
        UNPAIR_SESSION="$session" unpair_answered "$payload"
        stop_stub_daemon
        { [ "$CH_RC" = "$want_rc" ] && [ "$CH_OUT" = "$want_text" ]; } \
            || bad="$bad [$session $payload: rc=$CH_RC out=$CH_OUT]"
    done <<'EOF2'
alpha|{"outcome":"unpaired","folder":"/p/alpha","pairing":{"workspace":"T0AAAA","channel":"C0AAAA"}}|0|unpaired /p/alpha from C0AAAA in Slack workspace T0AAAA
alpha|{"outcome":"unpaired","folder":"/p/alpha","pairing":{"workspace":"T0AAAA","channel":"C0AAAA","name":"general"}}|0|unpaired /p/alpha from #general (C0AAAA) in Slack workspace T0AAAA
alpha|{"outcome":"unchanged","folder":"/p/alpha"}|0|unchanged: /p/alpha has no channel
alpha|{"outcome":"moved","folder":"/p/alpha"}|3|unknown: the pairing may have been made; running the same command again is safe and says what is there now
alpha|{"error":"the chat request failed","code":"handler_error"}|3|unknown: the pairing may have been made; running the same command again is safe and says what is there now
alpha|{"error":"unknown op: chat.row_pair"}|2|refused unsupported the computer that runs your sessions runs an older Ship of Tools, which lets only a person pair or unpair a project, in the Chat drawer's channel list (Ctrl+Shift+k, Cmd+Shift+k on a Mac); nothing was changed
alpha|{"error":"x","code":"stale_row"}|2|refused stale_row no session of the computer that runs your sessions is alpha, or its project folder cannot be reached (sot-fe version lists them as row <slug>)
.|{"error":"x","code":"stale_row"}|2|refused stale_row no session of the computer that runs your sessions is this session's project, or its project folder cannot be reached (sot-fe version lists them as row <slug>)
alpha|{"error":"x","code":"unmapped"}|2|refused unmapped alpha is the default session, which has no project folder
.|{"error":"x","code":"unmapped"}|2|refused unmapped this session's project is the default session, which has no project folder
alpha|{"error":"x","code":"unavailable"}|2|refused unavailable no chat is set up on the computer that runs your sessions: it is not the hub for the chat, or no Slack workspace is connected
alpha|{"error":"x","code":"not_saved"}|2|refused not_saved the chat's settings file chat.toml could not be written, or is refused as it stands; the log of Ship of Tools' service says why (sotd log-path prints where it is)
alpha|{"error":"x","code":"bad_request"}|2|refused bad_request the request is not valid (this session's handle is not its row's, its row is no longer there, or the session is named in over 256 bytes)
alpha|{"error":"the chat is busy","code":"busy"}|2|refused busy the chat is busy
alpha|{"error":"the chat request failed","code":"failed"}|2|refused failed the chat request failed
alpha|{"error":"words of a code added later","code":"other"}|2|refused other words of a code added later
EOF2
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

case_an_older_daemon_is_refused_unsupported() {
    UNPAIR_SESSION=alpha unpair_answered '{"error":"unknown op: chat.row_pair"}'
    local r; r="$(sent chat.row_pair)"; stop_stub_daemon
    [ "$CH_RC" = 2 ] && contains "$CH_OUT" "refused unsupported " && [ "$r" = 1 ] || { echo "rc=$CH_RC requests=$r out=$CH_OUT"; return 1; }
}

case_no_answer_to_a_pairing_is_one_request_and_exit_3() {
    start_stub_daemon
    CH_VERB=unpair run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- alpha
    local r; r="$(sent chat.row_pair)"; stop_stub_daemon
    [ "$CH_RC" = 3 ] && contains "$CH_OUT" "no answer: the pairing may have been made; running the same command again is safe and says what is there now" && [ "$r" = 1 ] \
        || { echo "rc=$CH_RC requests=$r out=$CH_OUT"; return 1; }
}

# --- chat pair -------------------------------------------------------------

# The row_pair answered with the JSON object $1; `pair <session> <channel>` run as the row, the handle alpha-h.
pair_answered() {
    stage_reply chat.row_pair "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.row_pair\",\"payload\":$1}"
    start_stub_daemon
    CH_VERB=pair run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- "${PAIR_SESSION:-alpha}" "${PAIR_CHANNEL:-#General}" ${PAIR_FLAGS:-}
}

case_pair_sends_one_row_pair_with_the_channel_as_given_and_prints_the_pairing() {
    local bad="" body r rs by_row by_name row ch ws keys
    for body in '{"workspace":"T0AAAA","workspace_tag":"lab","channel":"C0AAAA","name":"general","delivers":true}|paired /p/alpha with #general (C0AAAA) in lab (T0AAAA); new posts there reach that project'"'"'s session as mail' \
                '{"workspace":"T0AAAA","channel":"C0AAAA"}|paired /p/alpha with C0AAAA in Slack workspace T0AAAA'; do
        for flags in "" "--slack-workspace T0BBBB"; do
            for session in alpha .; do
                PAIR_SESSION="$session" PAIR_FLAGS="$flags" pair_answered "{\"outcome\":\"paired\",\"folder\":\"/p/alpha\",\"pairing\":${body%%|*}}"
                r="$(sent chat.row_pair)"; rs="$(sent chat.resolve)"
                by_row="$(paired_field .payload.by.row)"; by_name="$(paired_field .payload.by.name)"; row="$(paired_field .payload.row)"
                ch="$(paired_field '.payload.channel')"; ws="$(paired_field '.payload.workspace // "none"')"; keys="$(paired_field '.payload | keys | join(",")')"
                stop_stub_daemon
                local want_row="$session" want_ws=none want_keys="by,channel,row" want_out="${body#*|}"
                # This session's own project (`.`) reads "this project's"; another session's "that project's".
                [ "$session" != . ] || { want_row="$ROW"; want_out="${want_out//that project/this project}"; }
                [ -z "$flags" ] || { want_ws=T0BBBB; want_keys="by,channel,row,workspace"; }
                { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "$want_out" ] && [ "$r" = 1 ] && [ "$rs" = 0 ] && [ "$by_row" = "$ROW" ] && [ "$by_name" = alpha-h ] \
                    && [ "$row" = "$want_row" ] && [ "$ch" = '#General' ] && [ "$ws" = "$want_ws" ] && [ "$keys" = "$want_keys" ]; } \
                    || bad="$bad [$session '$flags': rc=$CH_RC out=$CH_OUT requests=$r resolves=$rs by=$by_row/$by_name row=$row channel=$ch ws=$ws keys=$keys]"
            done
        done
    done
    PAIR_SESSION=alpha PAIR_FLAGS="" pair_answered '{"outcome":"unchanged","folder":"/p/alpha","pairing":{"workspace":"T0AAAA","workspace_tag":"lab","channel":"C0AAAA","name":"general"}}'
    stop_stub_daemon
    { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "unchanged: /p/alpha already has #general (C0AAAA) in lab (T0AAAA)" ]; } || bad="$bad [unchanged: rc=$CH_RC out=$CH_OUT]"
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

# Whose project the mail words name: this session's own, by `.` or by its own id, else another's, by slug or by id.
case_pairing_this_sessions_own_project_says_this_projects_session() {
    local bad="" session whose
    for session in . "$ROW" alpha ws-other-3c4d; do
        PAIR_SESSION="$session" pair_answered '{"outcome":"paired","folder":"/p/alpha","pairing":{"workspace":"T0AAAA","channel":"C0AAAA","name":"general","workspace_tag":"lab","delivers":true}}'
        stop_stub_daemon
        whose=that; { [ "$session" = . ] || [ "$session" = "$ROW" ]; } && whose=this
        [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "paired /p/alpha with #general (C0AAAA) in lab (T0AAAA); new posts there reach $whose project's session as mail" ] \
            || bad="$bad [$session: rc=$CH_RC out=$CH_OUT]"
    done
    # A pairing that delivers nothing says nothing of mail.
    PAIR_SESSION=. pair_answered '{"outcome":"paired","folder":"/p/alpha","pairing":{"workspace":"T0AAAA","channel":"C0AAAA","name":"general","workspace_tag":"lab","delivers":false}}'
    stop_stub_daemon
    [ "$CH_OUT" = "paired /p/alpha with #general (C0AAAA) in lab (T0AAAA)" ] || bad="$bad [not delivering: out=$CH_OUT]"
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

case_each_pair_refusal_has_its_words_and_exit_code() {
    local bad="" want_rc want_text payload session
    while IFS='|' read -r session payload want_rc want_text; do
        PAIR_SESSION="$session" pair_answered "$payload"
        stop_stub_daemon
        { [ "$CH_RC" = "$want_rc" ] && [ "$CH_OUT" = "$want_text" ]; } \
            || bad="$bad [$session $payload: rc=$CH_RC out=$CH_OUT]"
    done <<'EOF2'
alpha|{"error":"x","code":"already_paired","workspace":"T0AAAA","workspace_tag":"lab","channel":"C0AAAA","name":"general"}|2|refused already_paired alpha already has #general (C0AAAA) in lab (T0AAAA); to move it, run sot-fe chat unpair alpha, then pair it again
.|{"error":"x","code":"already_paired","workspace":"T0AAAA","channel":"C0AAAA"}|2|refused already_paired this session's project already has C0AAAA in Slack workspace T0AAAA; to move it, run sot-fe chat unpair ., then pair it again
alpha|{"error":"x","code":"already_paired"}|2|refused already_paired alpha already has a channel; to move it, run sot-fe chat unpair alpha, then pair it again
alpha|{"error":"x","code":"already_paired","workspace":"T0AAAA","workspace_tag":"lab","channel":"C0AAAA","name":"general","joined":true}|2|refused already_paired alpha already has #general (C0AAAA) in lab (T0AAAA); to move it, run sot-fe chat unpair alpha, then pair it again; the Slack app is now in #General, and nothing was paired
alpha|{"error":"x","code":"channel_taken","joined":true}|2|refused channel_taken another project has that channel, or had it and its folder is gone; only a person can give it to this project, in the Chat drawer's channel list (Ctrl+Shift+k, Cmd+Shift+k on a Mac); the Slack app is now in #General, and nothing was paired
alpha|{"error":"x","code":"channel_taken"}|2|refused channel_taken another project has that channel, or had it and its folder is gone; only a person can give it to this project, in the Chat drawer's channel list (Ctrl+Shift+k, Cmd+Shift+k on a Mac)
alpha|{"error":"x","code":"several_channels","channels":["C0ONE","C0TWO"]}|2|refused several_channels several channels are named #General: C0ONE, C0TWO; name one by its ID
alpha|{"error":"x","code":"stale_row"}|2|refused stale_row no session of the computer that runs your sessions is alpha, or its project folder cannot be reached (sot-fe version lists them as row <slug>)
alpha|{"error":"x","code":"unmapped"}|2|refused unmapped alpha is the default session, which has no project folder
alpha|{"error":"x","code":"unavailable"}|2|refused unavailable the chat does not run on the computer that runs your sessions, or is stopped there
alpha|{"error":"x","code":"not_saved"}|2|refused not_saved the chat's settings file chat.toml could not be written, or is refused as it stands; the log of Ship of Tools' service says why (sotd log-path prints where it is)
alpha|{"error":"x","code":"bad_request"}|2|refused bad_request the request is not valid (this session's handle is not its row's, its row is no longer there, or the session or the channel is named in over 256 or 160 bytes, or with a control character)
alpha|{"error":"the chat is busy","code":"busy"}|2|refused busy the chat is busy
alpha|{"error":"Slack asked for a pause","code":"limited"}|2|refused limited Slack asked for a pause
alpha|{"error":"the chat request failed","code":"handler_error"}|3|unknown: the pairing may have been made; running the same command again is safe and says what is there now
alpha|{"error":"unknown op: chat.row_pair"}|2|refused unsupported the computer that runs your sessions runs an older Ship of Tools, which lets only a person pair a project, in the Chat drawer's channel list (Ctrl+Shift+k, Cmd+Shift+k on a Mac); nothing was changed
EOF2
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

# A channel the app cannot see or join: the workspace and the app are read once from chat.workspaces, and a hub of several
# workspaces lists them. Without that read the words still say what is true.
case_a_channel_not_found_on_a_hub_of_several_lists_the_workspaces_and_the_app() {
    local bad="" refusal flags body want w n
    local two='{"v":1,"id":4,"kind":"res","op":"chat.workspaces","payload":{"default":"T0AAAA","workspaces":[{"id":"T0AAAA","name":"Alpha Lab","running":true,"app_name":"alpha-app"},{"id":"T0BBBB","name":"Beta Lab","running":true,"app_name":"beta-app"}]}}'
    local one='{"v":1,"id":4,"kind":"res","op":"chat.workspaces","payload":{"default":"T0AAAA","workspaces":[{"id":"T0AAAA","running":true}]}}'
    local more="; this computer connects these Slack workspaces: T0AAAA (Alpha Lab), T0BBBB (Beta Lab): name another with --slack-workspace <id>"
    while IFS='|' read -r refusal flags w want; do
        stage_reply chat.row_pair "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.row_pair\",\"payload\":{\"error\":\"x\",\"code\":\"$refusal\"}}"
        case "$w" in two) stage_reply chat.workspaces "$two" ;; one) stage_reply chat.workspaces "$one" ;; esac
        start_stub_daemon
        CH_VERB=pair run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- alpha '#General' $flags
        n="$(sent chat.workspaces)"; stop_stub_daemon
        want="${want//MORE/$more}"
        { [ "$CH_RC" = 2 ] && [ "$CH_OUT" = "$want" ] && [ "$n" = 1 ]; } || bad="$bad [$refusal $flags $w: rc=$CH_RC reads=$n out=$CH_OUT]"
    done <<'EOF2'
no_such_channel||two|refused no_such_channel no channel #General that your Slack app can see in Alpha Lab (T0AAAA): a private channel shows only once the app is in it (in that channel in Slack, type /invite @alpha-app), and in a workspace of over 1,000 channels name it by its ID; to see the channels your Slack app can see, run sot-fe chat channelsMORE
no_such_channel|--slack-workspace T0BBBB|two|refused no_such_channel no channel #General that your Slack app can see in Beta Lab (T0BBBB): a private channel shows only once the app is in it (in that channel in Slack, type /invite @beta-app), and in a workspace of over 1,000 channels name it by its ID; to see the channels your Slack app can see, run sot-fe chat channels --slack-workspace T0BBBBMORE
not_in_channel||two|refused not_in_channel your Slack app is not in that channel and could not join it: in that channel in Slack, type /invite @alpha-app, then run this again; this computer connects these Slack workspaces: T0AAAA (Alpha Lab), T0BBBB (Beta Lab): name another with --slack-workspace <id>
no_such_channel||one|refused no_such_channel no channel #General that your Slack app can see in Slack workspace T0AAAA: a private channel shows only once the app is in it (in that channel in Slack, type /invite @your Slack app's name), and in a workspace of over 1,000 channels name it by its ID; to see the channels your Slack app can see, run sot-fe chat channels
unavailable|--slack-workspace T0ZZZZ|two|refused unavailable the Slack workspace T0ZZZZ is not connected on the computer that runs your sessionsMORE
unavailable|--slack-workspace T0BBBB|two|refused unavailable the chat does not run on the computer that runs your sessions, or is stopped there
not_in_channel||none|refused not_in_channel your Slack app is not in that channel and could not join it: in that channel in Slack, type /invite @your Slack app's name, then run this again
EOF2
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

# A name typed in capitals that begins with C or G reads as a channel ID, which no channel has: the refusal says so.
case_a_capitalized_name_read_as_an_id_says_so() {
    local one='{"v":1,"id":4,"kind":"res","op":"chat.workspaces","payload":{"default":"T0AAAA","workspaces":[{"id":"T0AAAA","running":true}]}}' bad="" channel want
    for channel in GENERAL CORE general; do
        stage_reply chat.row_pair '{"v":1,"id":3,"kind":"res","op":"chat.row_pair","payload":{"error":"x","code":"no_such_channel"}}'
        stage_reply chat.workspaces "$one"
        start_stub_daemon
        CH_VERB=pair run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- alpha "$channel"
        stop_stub_daemon
        want="refused no_such_channel no channel $channel that your Slack app can see in Slack workspace T0AAAA"
        [ "$channel" = general ] || want="$want; $channel was read as a channel ID, and a channel name is typed in lowercase"
        { [ "$CH_RC" = 2 ] && contains "$CH_OUT" "$want: a private channel shows only once" && { [ "$channel" != general ] || ! contains "$CH_OUT" "read as a channel ID"; }; } || bad="$bad [$channel: rc=$CH_RC out=$CH_OUT]"
    done
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

# --- chat workspaces and chat channels -------------------------------------

# One field of the logged request of op $1.
listed_field() { jq -r "select(.op==\"$1\") | $2" "$WORK/req-$STUBN.log"; }

WS_TWO='{"v":1,"id":4,"kind":"res","op":"chat.workspaces","payload":{"default":"T0AAAA","workspaces":[{"id":"T0AAAA","name":"Alpha Lab","running":true},{"id":"T0BBBB","running":false}]}}'
LISTING='{"workspace":"T0AAAA","channels":[{"id":"C0PUB","name":"general","private":false,"member":true,"taken":true},{"id":"C0FREE","name":"Random-Talk","private":false,"member":false},{"id":"G0IN","name":"inside","private":true,"member":true}]}'

# `chat workspaces` is one chat.workspaces and prints one line for each workspace; it sends no chat.channels.
case_workspaces_is_one_read_and_prints_one_line_each() {
    stage_reply chat.workspaces "$WS_TWO"; start_stub_daemon
    CH_VERB=workspaces run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h --
    local w c; w="$(sent chat.workspaces)"; c="$(sent chat.channels)"; stop_stub_daemon
    local want; want="$(printf '%s\n' 'T0AAAA default running Alpha Lab' 'T0BBBB other stopped (unnamed)')"
    { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "$want" ] && [ "$w" = 1 ] && [ "$c" = 0 ]; } || { echo "rc=$CH_RC out=$CH_OUT workspaces=$w channels=$c"; return 1; }
}

# A channels listing is ONE chat.channels with the row and the handle (and the workspace when named), then one read of the
# workspace's name; every channel is one line of the same five fields.
case_channels_sends_one_request_with_the_row_and_handle_and_prints_one_line_each() {
    local bad="" flags ch ws keys by_row by_name out want
    for flags in "" "--slack-workspace T0BBBB"; do
        stage_reply chat.channels "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.channels\",\"payload\":$LISTING}"
        stage_reply chat.workspaces "$WS_TWO"; start_stub_daemon
        CH_VERB=channels run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $flags
        ch="$(sent chat.channels)"; ws="$(listed_field chat.channels '.payload.workspace // "none"')"; keys="$(listed_field chat.channels '.payload | keys | join(",")')"
        by_row="$(listed_field chat.channels .payload.by.row)"; by_name="$(listed_field chat.channels .payload.by.name)"; stop_stub_daemon
        out="$CH_OUT"; want="$(printf '%s\n' 'workspace T0AAAA Alpha Lab' 'C0PUB #general public app-in taken' 'C0FREE #Random-Talk public app-out free' 'G0IN #inside private app-in free')"
        local want_ws=none want_keys="by"; [ -z "$flags" ] || { want_ws=T0BBBB; want_keys="by,workspace"; }
        { [ "$CH_RC" = 0 ] && [ "$out" = "$want" ] && [ "$ch" = 1 ] && [ "$by_row" = "$ROW" ] && [ "$by_name" = alpha-h ] && [ "$ws" = "$want_ws" ] && [ "$keys" = "$want_keys" ]; } \
            || bad="$bad [$flags: rc=$CH_RC out=$out requests=$ch by=$by_row/$by_name ws=$ws keys=$keys]"
    done
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

# The text narrows to the names that contain it, a leading # dropped and capitals ignored; the whole list prints without it.
case_channels_text_narrows_by_name_as_a_pairing_compares_names() {
    local bad="" text want
    while IFS='|' read -r text want; do
        stage_reply chat.channels "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.channels\",\"payload\":$LISTING}"
        stage_reply chat.workspaces "$WS_TWO"; start_stub_daemon
        CH_VERB=channels run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- "$text"
        stop_stub_daemon
        want="$(printf '%b' "workspace T0AAAA Alpha Lab\n$want")"
        { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "$want" ]; } || bad="$bad [$text: rc=$CH_RC out=$CH_OUT]"
    done <<'EOF2'
#GEN|C0PUB #general public app-in taken
random|C0FREE #Random-Talk public app-out free
Talk|C0FREE #Random-Talk public app-out free
n|C0PUB #general public app-in taken\nC0FREE #Random-Talk public app-out free\nG0IN #inside private app-in free
zzz|no channel name contains "zzz"
#|C0PUB #general public app-in taken\nC0FREE #Random-Talk public app-out free\nG0IN #inside private app-in free
EOF2
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

# A list Slack cut at its bound ends with one line that says so and that an ID still pairs; a workspace with no name, or one
# whose name could not be read, is the id alone; a workspace with no channel says so.
case_channels_says_when_the_list_was_cut_and_when_it_is_empty_or_the_name_is_unknown() {
    local bad="" cut='{"workspace":"T0AAAA","more":true,"channels":[{"id":"C0PUB","name":"general","private":false,"member":true}]}' empty='{"workspace":"T0BBBB","channels":[]}' out
    stage_reply chat.channels "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.channels\",\"payload\":$cut}"; start_stub_daemon
    CH_VERB=channels run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h --
    stop_stub_daemon
    out="$(printf '%s\n' 'workspace T0AAAA' 'C0PUB #general public app-in free' "Slack's list was cut after 1 channels; a channel not shown here can still be paired by its ID (the C or G code ending its link in Slack)")"
    { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "$out" ]; } || bad="$bad [cut: rc=$CH_RC out=$CH_OUT]"
    stage_reply chat.channels "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.channels\",\"payload\":$empty}"; stage_reply chat.workspaces "$WS_TWO"; start_stub_daemon
    CH_VERB=channels run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h --
    stop_stub_daemon
    out="$(printf '%s\n' 'workspace T0BBBB' 'your Slack app can see no channel here')"
    { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "$out" ]; } || bad="$bad [empty: rc=$CH_RC out=$CH_OUT]"
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

# Every failure of a read is exit 2 with its words, and a read changes nothing; an older daemon's answer says it runs an older
# Ship of Tools.
case_each_channels_refusal_has_its_words_and_exit_code() {
    local bad="" payload flags want n
    while IFS='|' read -r payload flags want; do
        stage_reply chat.channels "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.channels\",\"payload\":$payload}"
        stage_reply chat.workspaces "$WS_TWO"; start_stub_daemon
        CH_VERB=channels run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $flags
        n="$(sent chat.channels)"; stop_stub_daemon
        want="${want//MORE/; this computer connects these Slack workspaces: T0AAAA (Alpha Lab), T0BBBB: name another with --slack-workspace <id>}"
        { [ "$CH_RC" = 2 ] && [ "$CH_OUT" = "$want" ] && [ "$n" = 1 ]; } || bad="$bad [$payload $flags: rc=$CH_RC requests=$n out=$CH_OUT]"
    done <<'EOF2'
{"error":"the request is not valid","code":"bad_request"}||refused bad_request the computer that runs your sessions runs an older Ship of Tools, which cannot tell a session which channels its Slack app can see, or this session's handle is not its session's (sot-fe version lists the sessions)
{"error":"x","code":"stale_row"}||refused stale_row this session is gone from the computer that runs your sessions, or its project folder cannot be reached
{"error":"x","code":"unavailable"}||refused unavailable the chat does not run on the computer that runs your sessions, or is stopped there
{"error":"x","code":"unavailable"}|--slack-workspace T0BBBB|refused unavailable the chat does not run on the computer that runs your sessions, or is stopped there
{"error":"x","code":"unavailable"}|--slack-workspace T0ZZZZ|refused unavailable the Slack workspace T0ZZZZ is not connected on the computer that runs your sessionsMORE
{"error":"Slack asked for a pause; try again in a moment","code":"limited"}||refused limited Slack asked for a pause; try again in a moment
{"error":"the chat is busy; try again in a moment","code":"busy"}||refused busy the chat is busy; try again in a moment
{"error":"the chat request failed","code":"failed"}||refused failed the chat request failed
{"error":"unknown op: chat.channels"}||refused unsupported the computer that runs your sessions runs an older Ship of Tools, which cannot tell a session which channels its Slack app can see
EOF2
    # No answer at all is one request and exit 2: a read is never a post that may exist.
    start_stub_daemon
    CH_VERB=channels run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h --
    n="$(sent chat.channels)"; stop_stub_daemon
    { [ "$CH_RC" = 2 ] && contains "$CH_OUT" "refused: no answer from the computer that runs your sessions within 3s" && contains "$CH_OUT" "Nothing was changed." && [ "$n" = 1 ]; } || bad="$bad [no answer: rc=$CH_RC requests=$n out=$CH_OUT]"
    # The workspaces read: the same words for its failures.
    stage_reply chat.workspaces '{"v":1,"id":4,"kind":"res","op":"chat.workspaces","payload":{"error":"x","code":"unavailable"}}'; start_stub_daemon
    CH_VERB=workspaces run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h --
    stop_stub_daemon
    { [ "$CH_RC" = 2 ] && [ "$CH_OUT" = "refused unavailable the chat does not run on the computer that runs your sessions, or is stopped there" ]; } || bad="$bad [workspaces unavailable: rc=$CH_RC out=$CH_OUT]"
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

# Outside a session, or with no handle, nothing is sent; the post flags, a second text and a workspace that is no id are usage
# errors that send nothing; --slack-workspace is the pair and channels verbs' alone.
case_the_listing_verbs_are_refused_outside_a_session_and_on_bad_usage_sending_nothing() {
    local bad="" args envs n
    stage_reply chat.channels "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.channels\",\"payload\":$LISTING}"; stage_reply chat.workspaces "$WS_TWO"; start_stub_daemon
    for verb in channels workspaces; do
        for envs in "STUB_NAME=alpha-h" "SOT_WORKSPACE_ID=$ROW"; do
            # shellcheck disable=SC2086
            CH_VERB=$verb run_chat "" $envs --
            { [ "$CH_RC" = 2 ] && contains "$CH_OUT" "Nothing was sent"; } || bad="$bad [$verb $envs: rc=$CH_RC out=$CH_OUT]"
        done
    done
    for args in "channels a b" "channels --thread $TS" "channels --stdin" "channels --file x.png" "channels --slack-workspace bad" "channels --slack-workspace t0aaaa" \
                "workspaces extra" "workspaces --slack-workspace T0AAAA" "workspaces --stdin"; do
        # shellcheck disable=SC2086
        CH_VERB=${args%% *} run_chat "text" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- ${args#* }
        [ "$CH_RC" = 1 ] || bad="$bad [$args: rc=$CH_RC out=$CH_OUT]"
    done
    n="$(sent_any)"; stop_stub_daemon
    [ -z "$bad" ] && [ "$n" = 0 ] || { echo "$bad sent=$n"; return 1; }
}

# Outside a row or with no handle nothing is sent, and the post verb's flags, a missing or extra session and a flag
# of the pair verb that is not built here are usage errors that send nothing.
case_pair_verbs_refuse_the_post_flags() {
    local bad="" args envs n
    stage_reply chat.row_pair '{}'; start_stub_daemon
    for envs in "STUB_NAME=alpha-h" "SOT_WORKSPACE_ID=$ROW"; do
        # shellcheck disable=SC2086
        CH_VERB=unpair run_chat "" $envs -- alpha
        [ "$CH_RC" = 2 ] && contains "$CH_OUT" "Nothing was sent" || bad="$bad [$envs: rc=$CH_RC out=$CH_OUT]"
    done
    for args in "alpha --thread $TS" "alpha --stdin" "alpha --file x.png" "--file x.png alpha"; do
        # shellcheck disable=SC2086
        CH_VERB=unpair run_chat "text" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $args
        [ "$CH_RC" = 1 ] && contains "$CH_OUT" "ERROR: --thread, --file and --stdin are only for chat post" || bad="$bad [$args: rc=$CH_RC out=$CH_OUT]"
    done
    for args in "" "alpha beta" "alpha --slack-workspace T0AAAA"; do
        # shellcheck disable=SC2086
        CH_VERB=unpair run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $args
        [ "$CH_RC" = 1 ] || bad="$bad [$args: rc=$CH_RC out=$CH_OUT]"
    done
    # The pair verb: the post flags, a missing or extra argument, and a workspace that is no id, each exit 1; the workspace
    # flag is the pair verb's alone.
    for args in "alpha C0AAAA --thread $TS" "alpha C0AAAA --stdin" "alpha C0AAAA --file x.png" "" "alpha" "alpha C0AAAA C0BBBB" \
                "alpha C0AAAA --slack-workspace t0aaaa" "alpha C0AAAA --slack-workspace T0AAAA-X" "alpha C0AAAA --slack-workspace"; do
        # shellcheck disable=SC2086
        CH_VERB=pair run_chat "text" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $args
        [ "$CH_RC" = 1 ] || bad="$bad [pair $args: rc=$CH_RC out=$CH_OUT]"
    done
    CH_VERB=post run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- hi --slack-workspace T0AAAA
    { [ "$CH_RC" = 1 ] && contains "$CH_OUT" "--slack-workspace is only for chat pair"; } || bad="$bad [post --slack-workspace: rc=$CH_RC out=$CH_OUT]"
    CH_VERB=pair run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- alpha C0AAAA --slack-workspace bad
    { [ "$CH_RC" = 1 ] && contains "$CH_OUT" "ERROR: --slack-workspace expects a Slack workspace ID like T0123ABCD, got 'bad'"; } || bad="$bad [bad workspace: rc=$CH_RC out=$CH_OUT]"
    n="$(sent_any)"; stop_stub_daemon
    [ -z "$bad" ] && [ "$n" = 0 ] || { echo "$bad sent=$n"; return 1; }
}

# --- chat read -------------------------------------------------------------

RLINE1='1700000100.000100 top replies=2 person "Ada Example" files=none | can you plot the run?'
RLINE2='1700000500.000400 reply thread=1700000100.000100 session "alpha-h" files=F0FILE:"fit.png" | the fit'
# The row mapped and the read answered with the JSON object $1; the verb run with the arguments after it.
read_answered() {
    local reply="$1"; shift
    stage_reply chat.resolve "$MAPPED"
    stage_reply chat.read "{\"v\":1,\"id\":3,\"kind\":\"res\",\"op\":\"chat.read\",\"payload\":$reply}"
    start_stub_daemon
    CH_VERB=read run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- "$@"
}
read_field() { jq -r "select(.op==\"chat.read\") | $1" "$WORK/req-$STUBN.log"; }
# The lines one reply prints, as the verb is run with the arguments; they are the daemon's, as sent.
case_read_is_one_resolve_then_one_read_with_the_handle_and_prints_the_daemons_lines() {
    local bad="" args want r d first keys ids got
    while IFS='|' read -r args want; do
        # shellcheck disable=SC2086
        read_answered "$(jq -nc --arg a "$RLINE1" --arg b "$RLINE2" '{lines:[$a,$b]}')" $args
        r="$(sent chat.resolve)"; d="$(sent chat.read)"
        first="$(jq -r 'select(.op=="chat.resolve" or .op=="chat.read") | .op' "$WORK/req-$STUBN.log" | head -n 1)"
        keys="$(read_field '.payload | keys | join(",")')"
        ids="$(read_field '[.payload.target.row, .payload.target.workspace, .payload.target.channel, .payload.name] | join(" ")')"
        got="$(read_field '[.payload.thread // "-", .payload.before // "-"] | join(" ")')"
        stop_stub_daemon
        { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "$RLINE1"$'\n'"$RLINE2" ] && [ "$r" = 1 ] && [ "$d" = 1 ] && [ "$first" = chat.resolve ] \
            && [ "$ids" = "$ROW T0AAAA C0AAAA alpha-h" ] && [ "$keys;$got" = "$want" ]; } \
            || bad="$bad [$args: rc=$CH_RC resolve=$r read=$d first=$first ids=$ids keys;got=$keys;$got out=$CH_OUT]"
    done <<'EOF'
|name,target;- -
--thread 1700000050.000000|name,target,thread;1700000050.000000 -
--before 1700000900.000100|before,name,target;- 1700000900.000100
--thread 1700000050.000000 --before 1700000900.000100|before,name,target,thread;1700000050.000000 1700000900.000100
EOF
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

case_read_says_where_older_messages_begin_and_when_there_are_none() {
    local bad="" args filter want
    # The reply is a jq filter over $a and $b (the two lines), the wanted output has @1 and @2 for them and \n for a line end.
    while IFS='~' read -r args filter want; do
        want="${want//@1/$RLINE1}"; want="${want//@2/$RLINE2}"
        # shellcheck disable=SC2086
        read_answered "$(jq -nc --arg a "$RLINE1" --arg b "$RLINE2" "$filter")" $args
        stop_stub_daemon
        { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "$(printf '%b' "$want")" ]; } || bad="$bad [$args: rc=$CH_RC out=$CH_OUT]"
    done <<'EOF'
~{lines:[$a],more:"1700000100.000100"}~@1\nmore: older messages are not shown; to read them, run sot-fe chat read --before 1700000100.000100
--thread 1700000050.000000~{lines:[$b],more:"1700000500.000400",limited:true}~@2\nolder messages are hidden by Slack's plan for this workspace\nmore: older messages are not shown; to read them, run sot-fe chat read --thread 1700000050.000000 --before 1700000500.000400
~{lines:[]}~no message in this channel
--before 1700000900.000100~{lines:[]}~no message in this channel before 1700000900.000100
--thread 1700000050.000000~{lines:[]}~no message in thread 1700000050.000000
--thread 1700000050.000000 --before 1700000900.000100~{lines:[],limited:true}~no message in thread 1700000050.000000 before 1700000900.000100\nolder messages are hidden by Slack's plan for this workspace
~{lines:[$a],more:"not a time"}~@1
EOF
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

case_each_read_refusal_has_its_words_and_exit_code() {
    local bad="" reply want d
    while IFS='|' read -r reply want; do
        read_answered "$reply"
        stop_stub_daemon
        { [ "$CH_RC" = 2 ] && [ "$CH_OUT" = "$want" ]; } || bad="$bad [$reply: rc=$CH_RC out=$CH_OUT]"
    done <<'EOF'
{"error":"unknown op: chat.read"}|refused unsupported the computer that runs your sessions runs an older Ship of Tools, which cannot read the channel back
{"error":"the request is not valid","code":"bad_request"}|refused bad_request the request is not valid (this session's handle is not its session's, or a time is not a message's time)
{"error":"the session's project now has another channel","code":"stale_mapping"}|refused stale_mapping the session's project now has another channel
{"error":"the chat is not available here","code":"unavailable"}|refused unavailable the chat is not available here
{"error":"the app is not in the channel","code":"not_in_channel"}|refused not_in_channel the app is not in the channel
{"error":"the chat request failed","code":"handler_error"}|refused handler_error the chat request failed
EOF
    # No answer is exit 2, never 3: a read changes nothing. One read request, never a second.
    stage_reply chat.resolve "$MAPPED"; start_stub_daemon
    CH_VERB=read run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h --
    d="$(sent chat.read)"; stop_stub_daemon
    { [ "$CH_RC" = 2 ] && contains "$CH_OUT" "refused: no answer from the computer that runs your sessions within 3s" && contains "$CH_OUT" "Nothing was read." && [ "$d" = 1 ]; } \
        || bad="$bad [no answer: rc=$CH_RC reads=$d out=$CH_OUT]"
    # A resolve that maps nothing reads nothing, and a resolve with no answer says so in a read's words.
    stage_reply chat.resolve '{"v":1,"id":2,"kind":"res","op":"chat.resolve","payload":{"state":"unmapped"}}'; start_stub_daemon
    CH_VERB=read run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h --
    d="$(sent chat.read)"; stop_stub_daemon
    { [ "$CH_RC" = 2 ] && [ "$CH_OUT" = "refused unmapped this session's project has no channel" ] && [ "$d" = 0 ]; } || bad="$bad [unmapped: rc=$CH_RC reads=$d out=$CH_OUT]"
    start_stub_daemon
    CH_VERB=read run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h --
    d="$(sent chat.read)"; stop_stub_daemon
    { [ "$CH_RC" = 2 ] && contains "$CH_OUT" "Nothing was read." && [ "$d" = 0 ]; } || bad="$bad [no resolve answer: rc=$CH_RC reads=$d out=$CH_OUT]"
    [ -z "$bad" ] || { echo "$bad"; return 1; }
}

case_read_usage_errors_send_nothing_and_exit_1() {
    local bad="" envs n args
    stage_reply chat.resolve "$MAPPED"; stage_reply chat.read '{}'; start_stub_daemon
    for envs in "STUB_NAME=alpha-h" "SOT_WORKSPACE_ID=$ROW"; do
        # shellcheck disable=SC2086
        CH_VERB=read run_chat "" $envs --
        [ "$CH_RC" = 2 ] && contains "$CH_OUT" "Nothing was sent" || bad="$bad [$envs: rc=$CH_RC out=$CH_OUT]"
    done
    for args in "hello" "--stdin" "--file x.png" "--slack-workspace T0AAAA" "--thread notats" "--thread 17.5" "--thread 1700000050.00000" "--thread 1700000050.0000000" \
                "--before notats" "--before 17.5" "--before 1700000050.0000000" "--before" "--thread 1700000050.000000 extra"; do
        # shellcheck disable=SC2086
        CH_VERB=read run_chat "text" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- $args
        [ "$CH_RC" = 1 ] || bad="$bad [$args: rc=$CH_RC out=$CH_OUT]"
    done
    # --before is the read's alone.
    CH_VERB=post run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- hi --before 1700000050.000100
    { [ "$CH_RC" = 1 ] && contains "$CH_OUT" "ERROR: --before is only for chat read"; } || bad="$bad [post --before: rc=$CH_RC out=$CH_OUT]"
    CH_VERB=delete run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- 1700000000.000100 --before 1700000050.000100
    { [ "$CH_RC" = 1 ] && contains "$CH_OUT" "ERROR: --before is only for chat read"; } || bad="$bad [delete --before: rc=$CH_RC out=$CH_OUT]"
    n="$(sent_any)"; stop_stub_daemon
    [ -z "$bad" ] && [ "$n" = 0 ] || { echo "$bad sent=$n"; return 1; }
}

# A line the daemon sent with a control byte, an escape or a line break in it prints as one line: nothing of it reaches the
# terminal as a control, and no line starts where the daemon's did not.
case_a_read_line_with_a_control_byte_prints_as_one_line() {
    local forged want
    forged="$(printf '1700000100.000100 top person "x" files=none | a\033[2Jb\007c\r\nforged line\001d\te\177f')"
    read_answered "$(jq -nc --arg a "$forged" --arg b "$RLINE2" '{lines:[$a,$b]}')"
    stop_stub_daemon
    want="$(printf '1700000100.000100 top person "x" files=none | a [2Jb c  forged line d e f\n%s' "$RLINE2")"
    { [ "$CH_RC" = 0 ] && [ "$CH_OUT" = "$want" ] && [ "$(printf '%s\n' "$CH_OUT" | wc -l | tr -d ' ')" = 2 ]; } \
        || { echo "rc=$CH_RC out=$(printf '%s' "$CH_OUT" | od -c | head -8)"; return 1; }
}

# The read's wait is past the daemon's own bounds for it, read from sot-chat's constants, by at least 5 s: the names'
# and the read's deadlines.
case_the_read_wait_is_past_the_daemons_own_bounds() {
    local api="$SCRIPT_DIR/../../rust/chat/src/api.rs" chat="$SCRIPT_DIR/../sot-fe/sot-fe-chat.sh" read names wait
    secs() { sed -n "s/^pub const $1: Duration = Duration::from_secs(\([0-9]*\));.*/\1/p" "$api"; }
    read="$(secs READ_DEADLINE)"; names="$(secs NAMES_DEADLINE)"
    wait="$(sed -n '/^send_chat_read()/,/^}/s/.*TIMEOUT:-\([0-9]*\)}.*/\1/p' "$chat")"
    [ -n "$read" ] && [ -n "$names" ] && [ -n "$wait" ] && [ "$wait" -ge $((names + read + 5)) ] \
        || { echo "read=$read names=$names wait=$wait"; return 1; }
}

# A post that ended with no answer, into a thread, names that thread in the read it points to.
case_no_answer_to_a_post_in_a_thread_points_to_that_threads_read() {
    stage_reply chat.resolve "$MAPPED"; start_stub_daemon
    run_chat "" SOT_WORKSPACE_ID=$ROW STUB_NAME=alpha-h -- --thread 1700000050.000000 "hi"
    stop_stub_daemon
    [ "$CH_RC" = 3 ] && contains "$CH_OUT" "no answer: the post may exist; do not send it again; to see whether it went out, run sot-fe chat read --thread 1700000050.000000" \
        || { echo "rc=$CH_RC out=$CH_OUT"; return 1; }
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
check "a file post with no message time says how to find it, with and without a thread" case_a_file_post_with_no_time_says_how_to_find_it
check "no answer to the file post is one request, never two, and exit 3"                    case_no_answer_to_the_file_post_is_one_request_and_exit_3
check "a file usage error sends nothing and exits 1"                                       case_a_file_usage_error_sends_nothing_and_exits_1
check "sot-fe prints wire's report for every case of told_words_cases.tsv"                case_sot_fe_prints_the_wire_report_for_every_case
check "the mentioned report splits back into exactly the entries sent"                      case_the_mentioned_report_splits_back_into_the_entries_sent
check "each wait is past the daemon's own bounds for that post, with a margin"             case_each_wait_is_past_the_daemons_own_bounds
check "a delete outside a row, or with no comm handle, sends nothing"                       case_a_delete_outside_a_row_or_with_no_handle_sends_nothing
check "a delete is one chat.resolve, then one chat.delete_post, and prints each file"      case_delete_sends_one_delete_post_and_prints_each_file
check "each delete_post answer prints its words and exits with its code"                    case_each_delete_post_answer_has_its_words_and_exit_code
check "a delete takes a thread, and refuses one that is no ts"                              case_delete_takes_a_thread_and_refuses_one_that_is_no_ts
check "an older daemon cannot delete a post with its files, and the older op is not tried"  case_an_older_daemon_cannot_delete_a_post_with_its_files
check "a delete whose resolve maps nothing deletes nothing"                                 case_a_delete_that_maps_nothing_deletes_nothing
check "no answer to the delete is one request, never two, and exit 3"                       case_no_answer_to_the_delete_is_one_request_and_exit_3
check "no answer to the resolve deletes nothing, in plain words"                            case_no_answer_to_the_resolve_deletes_nothing
check "a delete usage error sends nothing and exits 1"                                      case_a_delete_usage_error_sends_nothing_and_exits_1
check "the delete's wait is past the daemon's own bounds, with a margin"                    case_the_delete_wait_is_past_the_daemons_own_bounds

check "an unpair is one chat.row_pair with the row, the handle and a null channel, and prints what it removed" case_unpair_sends_one_row_pair_with_the_handle_and_prints_what_it_removed
check "each unpair answer prints its words and exits with its code"                         case_each_unpair_answer_has_its_words_and_exit_code
check "an older daemon's unknown op is refused as unsupported, with exit 2"                  case_an_older_daemon_is_refused_unsupported
check "no answer to a pairing is one request, never two, and exit 3"                        case_no_answer_to_a_pairing_is_one_request_and_exit_3
check "a pair is one chat.row_pair with the channel as given and the workspace when named, and prints the pairing" case_pair_sends_one_row_pair_with_the_channel_as_given_and_prints_the_pairing
check "a pairing of this session's own project says this project's session, another's that project's" case_pairing_this_sessions_own_project_says_this_projects_session
check "an unpairing of a delivering channel says its posts no longer reach the session as mail" case_unpairing_a_delivering_channel_says_its_posts_stop_reaching_the_session
check "each pair refusal prints its words and exits with its code"                          case_each_pair_refusal_has_its_words_and_exit_code
check "a channel not found lists the workspaces and the app from one chat.workspaces read" case_a_channel_not_found_on_a_hub_of_several_lists_the_workspaces_and_the_app
check "a capitalized name read as an ID says so in the refusal" case_a_capitalized_name_read_as_an_id_says_so
check "the pairing verbs refuse the post flags and a missing row or handle, sending nothing" case_pair_verbs_refuse_the_post_flags
check "the workspaces verb is one chat.workspaces and prints one line for each workspace" case_workspaces_is_one_read_and_prints_one_line_each
check "a channels listing is one chat.channels with the row and the handle, and prints one line of five fields for each channel" case_channels_sends_one_request_with_the_row_and_handle_and_prints_one_line_each
check "the text narrows a listing by name as a pairing compares names"                      case_channels_text_narrows_by_name_as_a_pairing_compares_names
check "a listing says when the list was cut, when it is empty, and prints an unknown name as the id" case_channels_says_when_the_list_was_cut_and_when_it_is_empty_or_the_name_is_unknown
check "each listing failure prints its words and exits 2, a read never being a post that may exist" case_each_channels_refusal_has_its_words_and_exit_code
check "the listing verbs are refused outside a session and on bad usage, sending nothing"    case_the_listing_verbs_are_refused_outside_a_session_and_on_bad_usage_sending_nothing
check "a read is one chat.resolve, then one chat.read with the handle, and prints the daemon's lines" case_read_is_one_resolve_then_one_read_with_the_handle_and_prints_the_daemons_lines
check "a read says where older messages begin, what Slack's plan hides, and when there are none" case_read_says_where_older_messages_begin_and_when_there_are_none
check "each read refusal prints its words and exits 2, a read never being a post that may exist" case_each_read_refusal_has_its_words_and_exit_code
check "a read outside a session or with a usage error sends nothing and exits 1"            case_read_usage_errors_send_nothing_and_exit_1
check "a read line with a control byte or a line break prints as one line"                  case_a_read_line_with_a_control_byte_prints_as_one_line
check "the read's wait is past the daemon's own bounds, with a margin"                       case_the_read_wait_is_past_the_daemons_own_bounds
check "no answer to a post in a thread points to that thread's read"                         case_no_answer_to_a_post_in_a_thread_points_to_that_threads_read

echo ""
echo "$PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
