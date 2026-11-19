# sot-fe's reply in the row's channel: `chat post`, one `chat.post` (or, with --file, one `chat.post_file`) of kind `row`
# into the current thread.
# Sourced by sot-fe; defines functions only.

# The row's own handle, as comm-context.sh gives it ("" when the session has not joined). In its own subshell, never
# `eval`d into this shell.
_chat_handle() {
    ( eval "$("$SCRIPT_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null; printf '%s' "${NAME:-}") 2>/dev/null || true
}

# A refusal's words: the daemon's own `error` for a refused op, else the fixed words of the three codes a refused post
# carries. Prints "refused <code> <words>" and exits 2.
_chat_refused() {
    local code="$1" words="${2:-}"
    if [ -z "$words" ]; then
        case "$code" in
            busy)        words="the chat is busy; try again in a moment" ;;
            bad_request) words="the request is not valid (empty or over-long text, or a name that is not this session's)" ;;
            failed)      words="the chat request failed" ;;
        esac
    fi
    echo "refused $code${words:+ $words}" >&2
    exit 2
}

# The post may exist: exit 3, the one code that means "never post again". Every exit 2 is a certain "nothing was posted".
# (A hello the daemon refused, or a frame that never left, is no answer too, and says the same: it cannot be told apart.)
_chat_may_exist() {
    echo "$1: the post may exist; do not send it again; ask the person to look in the channel" >&2
    exit 3
}

# One request, one reply, no second try: the chat's post is submitted once (chat guarantee 12), so unlike
# `send_fe_command` nothing here sends a frame again. $1=op, $2=the request frame.
_chat_ask() {
    sot_send "$2" "$1" || true
}

# The row this session runs in ($SOT_WORKSPACE_ID) and its comm handle, into CHAT_ROW and CHAT_HANDLE; refused (exit 2)
# with nothing sent when either is missing. Runs before any endpoint is found.
_chat_identity() {
    CHAT_ROW="${SOT_WORKSPACE_ID:-}"
    if [ -z "$CHAT_ROW" ]; then
        echo "refused: SOT_WORKSPACE_ID is unset, so this is not a Ship of Tools session and has no channel. Nothing was sent." >&2
        exit 2
    fi
    CHAT_HANDLE="$(_chat_handle)"
    if [ -z "$CHAT_HANDLE" ]; then
        echo "refused: this session has no comm handle (not joined, or its handle is another session's). Nothing was sent." >&2
        exit 2
    fi
}

# One `chat.resolve` for $CHAT_ROW, its mapped pair into CHAT_PAIR (the reply's payload); any other answer is refused
# (exit 2), and nothing is posted.
_chat_resolve() {
    local req resp err state
    _need_endpoint
    export SEND_TIMEOUT="${TIMEOUT:-10}"
    # The row is id-shaped (a workspace id).
    req="$(jq -nc --arg w "$CHAT_ROW" '{v:1, id:2, kind:"req", op:"chat.resolve", payload:{row:$w}}')"
    resp="$(_chat_ask chat.resolve "$req")"
    if [ -z "$resp" ]; then
        echo "refused: no answer to chat.resolve within ${SEND_TIMEOUT}s via $ENDPOINT. Nothing was posted." >&2
        exit 2
    fi
    err="$(printf '%s' "$resp" | jq -r '.payload.error // empty')"
    if [ -n "$err" ]; then
        _chat_refused "$(printf '%s' "$resp" | jq -r '.payload.code // "?"')" "$err"
    fi
    state="$(printf '%s' "$resp" | jq -r '.payload.state // empty')"
    case "$state" in
        mapped) : ;;
        unmapped) _chat_refused unmapped "this session's project has no channel" ;;
        unconfigured)
            # The daemon's reason, in words that say what is wrong (a connect from the window's Chat drawer fixes each).
            case "$(printf '%s' "$resp" | jq -r '.payload.reason // empty')" in
                not_connected) _chat_refused unconfigured "no Slack workspace is connected on the computer that runs your sessions" ;;
                tokens_refused) _chat_refused unconfigured "Slack refused the connected tokens" ;;
                wrong_app) _chat_refused unconfigured "the connected Slack tokens belong to two different apps" ;;
                start_failed) _chat_refused unconfigured "the chat could not start" ;;
                *) _chat_refused unconfigured "the chat is not set up on the computer that runs your sessions" ;;
            esac ;;
        *) _chat_refused invalid "the chat's configuration does not map this session's project" ;;
    esac
    CHAT_PAIR="$(printf '%s' "$resp" | jq -c '.payload')"
}

# The one post's reply $1 (empty for none): exits 0 with `posted <ts>` (or `posted file <id>` for a file share Slack
# named no message of), followed by `; mentioned <names>` for the members the post mentions and `; not linked:
# @<name> (<why>[: <candidates>])` for each `@` name the daemon left as text (the words are wire's
# `ChatUnlinkedWhy::text`; an empty name is `and more`), 2 when nothing was posted, 3 when the post may exist.
_chat_posted() {
    local resp="$1" err code
    if [ -z "$resp" ]; then
        _chat_may_exist "no answer"
    fi
    err="$(printf '%s' "$resp" | jq -r '.payload.error // empty')"
    if [ -n "$err" ]; then
        code="$(printf '%s' "$resp" | jq -r '.payload.code // "?"')"
        # A panic in the daemon's job after the send began is an error reply too: the post may exist.
        [ "$code" != handler_error ] || _chat_may_exist unknown
        _chat_refused "$code" "$err"
    fi
    case "$(printf '%s' "$resp" | jq -r '.payload.outcome // empty')" in
        posted)
            printf '%s' "$resp" | jq -r '{no_member: "no member", several_members: "several members",
                members_unread: "the member list could not be read", too_many_members: "the member list is too large",
                channel_wide: "a session never notifies a whole channel", too_long: "the post would be too long"} as $why
                | def names: map(if . == "" then "and more" else . end) | join(", ");
                (if .payload.ts then "posted \(.payload.ts)" else "posted file \(.payload.file)" end)
                + ((.payload.mentioned // []) | if length > 0 then "; mentioned " + names else "" end)
                + ([.payload.unlinked[]? | if .why == "more" then "and more"
                    else "@\(.name) (\($why[.why] // "not linked")\(if (.candidates // []) | length > 0 then ": " + (.candidates | names) else "" end))" end]
                   | if length > 0 then "; not linked: " + join(", ") else "" end)'
            ;;
        refused)
            _chat_refused "$(printf '%s' "$resp" | jq -r '.payload.code // "?"')"
            ;;
        limited)
            printf '%s' "$resp" | jq -r '"limited, wait \(.payload.retry_after_secs) s"' >&2
            exit 2
            ;;
        *)
            _chat_may_exist unknown
            ;;
    esac
}

# $1=text, $2=thread ts or "". Posts for the row this session runs in ($SOT_WORKSPACE_ID) under its comm handle, into
# the thread, and exits 0 with `posted <ts>`; 2 when nothing was posted (a refusal or a limit); 3 when the post may exist.
send_chat_post() {
    local text="$1" thread="$2" req
    _chat_identity
    _chat_resolve
    # The text, the handle and the thread are free text and reach jq on stdin, as JSON strings after the resolved
    # pair (comm-lib-base.sh, free text and jq); the row is the id above.
    req="$({ printf '%s\n' "$CHAT_PAIR"; sot_json_escape "$CHAT_ROW"; sot_json_escape "$CHAT_HANDLE"; sot_json_escape "$thread"; sot_json_escape "$text"; } \
        | jq -cn 'input as $r | input as $row | input as $name | input as $thread | input as $text
            | {v:1, id:3, kind:"req", op:"chat.post",
               payload: ({target:{row:$row, workspace:$r.workspace, channel:$r.channel}, kind:"row", name:$name, text:$text}
                         + (if $thread == "" then {} else {thread:$thread} end))}')"
    # Past the daemon's own bounds with a margin, so a made post or a refusal is never read as "may exist": the member
    # list's 10 s (a name to link), then the post's 30 s (sot-chat's NAMES_DEADLINE and POST_DEADLINE).
    export SEND_TIMEOUT="${TIMEOUT:-50}"
    _chat_posted "$(_chat_ask chat.post "$req")"
}

# The node id (`files:<rel>`) of the file at $1, a path in the row's folder: a relative one is joined to $PWD, and the
# result is made relative to $SOT_WORKSPACE_ROOT by comm-lib's sot_relative_to (one spelling of both, so Git Bash's
# /c/... meets a C:\... root, and a link above the folder is resolved, never one inside it); a path outside it, the
# folder itself, or one with a `..` part prints nothing and returns 1. The daemon reads the file itself.
_chat_node_id() {
    local path="$1" rel
    [ -n "${SOT_WORKSPACE_ROOT:-}" ] || return 1
    case "$path" in /*|[A-Za-z]:[\\/]*) : ;; *) path="$PWD/$path" ;; esac
    rel="$(sot_relative_to "$SOT_WORKSPACE_ROOT" "$path")" || return 1
    case "/$rel/" in //|/./|*/../*) return 1 ;; esac
    printf 'files:%s' "$rel"
}

# $1=file path, $2=comment or "", $3=thread ts or "". Shares the file for the row this session runs in under its comm
# handle, with the comment after the row's label, into the thread: exits 0 with `posted <ts>` or `posted file <id>`; 2
# when nothing was shared; 3 when the share may exist.
send_chat_post_file() {
    local file="$1" comment="$2" thread="$3" node req
    _chat_identity
    if ! node="$(_chat_node_id "$file")"; then
        echo "refused: '$file' is not a file under this session's project folder (${SOT_WORKSPACE_ROOT:-SOT_WORKSPACE_ROOT is unset}). Nothing was sent." >&2
        exit 2
    fi
    _chat_resolve
    # The node id, the handle, the thread and the comment are free text and reach jq on stdin, as in send_chat_post.
    req="$({ printf '%s\n' "$CHAT_PAIR"; sot_json_escape "$CHAT_ROW"; sot_json_escape "$CHAT_HANDLE"; sot_json_escape "$thread"; sot_json_escape "$node"; sot_json_escape "$comment"; } \
        | jq -cn 'input as $r | input as $row | input as $name | input as $thread | input as $node | input as $comment
            | {v:1, id:3, kind:"req", op:"chat.post_file",
               payload: ({target:{row:$row, workspace:$r.workspace, channel:$r.channel}, kind:"row", name:$name, node_id:$node}
                         + (if $thread == "" then {} else {thread:$thread} end)
                         + (if $comment == "" then {} else {comment:$comment} end))}')"
    # Past the daemon's own bounds with a margin: the member list's 10 s (a name in the comment), the upload URL's read
    # 15 s, the upload's 60 s and the share's 30 s (sot-chat's NAMES_, READ_, TRANSFER_ and POST_DEADLINE), 115 s.
    export SEND_TIMEOUT="${TIMEOUT:-130}"
    _chat_posted "$(_chat_ask chat.post_file "$req")"
}
