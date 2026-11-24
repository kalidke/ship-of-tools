# sot-fe's reply in the row's channel: `chat post`, one `chat.post` (or, with --file, one `chat.post_file`) of kind `row`
# into the current thread; `chat delete`, one `chat.delete` of a message the hub's Slack app posted there; and
# `chat unpair`, one `chat.row_pair` that removes the channel of a project folder.
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
            not_own_message) words="Slack would not let this project's Slack app delete that message (a person's or another app's message stays)" ;;
            unavailable) words="the chat is not available here; nothing was sent" ;;
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
# (exit 2), and nothing is posted (nothing is "$CHAT_DONE" when a verb sets it).
_chat_resolve() {
    local req resp err state
    _need_endpoint
    export SEND_TIMEOUT="${TIMEOUT:-10}"
    # The row is id-shaped (a workspace id).
    req="$(jq -nc --arg w "$CHAT_ROW" '{v:1, id:2, kind:"req", op:"chat.resolve", payload:{row:$w}}')"
    resp="$(_chat_ask chat.resolve "$req")"
    if [ -z "$resp" ]; then
        echo "refused: no answer from the computer that runs your sessions within ${SEND_TIMEOUT}s (via $ENDPOINT). Nothing was ${CHAT_DONE:-posted}." >&2
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
# named no message of), followed by the report wire's `told_words` makes (`; mentioned <n>: <entries>`, `; not linked:
# <name> (<why>[, <n>: <candidates>])`, entries printed as the daemon made them, `"display" ("real") @handle` and
# `"@name"`, where a double quote is structure; the count is the people a list names, `<n> and more` with `, and more`
# when an empty last name says there were more), 2 when nothing was posted, 3 when the post may exist.
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
                | def names: (map(select(. != "")) | length) as $n | (if $n < length then " and more" else "" end) as $more
                    | "\($n)\($more): " + (map(select(. != "")) | join(", ")) + (if $more == "" then "" else ", and more" end);
                (if .payload.ts then "posted \(.payload.ts)" else "posted file \(.payload.file)" end)
                + ((.payload.mentioned // []) | if length > 0 then "; mentioned " + names else "" end)
                + ([.payload.unlinked[]? | if .why == "more" then "and more"
                    else "\(.name) (\($why[.why] // "not linked")\(if (.candidates // []) | length > 0 then ", " + (.candidates | names) else "" end))" end]
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

# $1=the message's ts, which sot-fe has checked is `<seconds>.<six digits>`. Deletes that message of the channel mapped to
# the row this session runs in, under its comm handle: ONE `chat.delete`, never sent again. Exits 0 with `deleted <ts>`
# or `gone <ts>: ...` (the channel has no such message); 2 when nothing was deleted; 3 when the message may be deleted (no answer, unknown).
send_chat_delete() {
    local ts="$1" req
    CHAT_DONE=deleted
    _chat_identity
    _chat_resolve
    req="$({ printf '%s\n' "$CHAT_PAIR"; sot_json_escape "$CHAT_ROW"; sot_json_escape "$CHAT_HANDLE"; } \
        | jq -cn --arg ts "$ts" 'input as $r | input as $row | input as $name
            | {v:1, id:3, kind:"req", op:"chat.delete",
               payload: {target:{row:$row, workspace:$r.workspace, channel:$r.channel}, name:$name, ts:$ts}}')"
    # Past the daemon's own bound with a margin: the delete's 30 s (sot-chat's POST_DEADLINE).
    export SEND_TIMEOUT="${TIMEOUT:-45}"
    _chat_deleted "$(_chat_ask chat.delete "$req")" "$ts"
}

# The delete's reply $1 (empty for none) for the message $2, as `_chat_posted` reads a post's.
_chat_deleted() {
    local resp="$1" ts="$2" err code
    local maybe="the message may have been deleted; run the same command again, which tells what is true now"
    if [ -z "$resp" ]; then
        echo "no answer: $maybe" >&2
        exit 3
    fi
    err="$(printf '%s' "$resp" | jq -r '.payload.error // empty')"
    if [ -n "$err" ]; then
        case "$err" in
            "unknown op"*)
                echo "refused: the computer that runs your sessions runs an older Ship of Tools that cannot delete chat messages. Nothing was deleted." >&2
                exit 2 ;;
        esac
        code="$(printf '%s' "$resp" | jq -r '.payload.code // "?"')"
        # A panic in the daemon's job after the send began is an error reply too: the message may be deleted.
        [ "$code" != handler_error ] || { echo "unknown: $maybe" >&2; exit 3; }
        _chat_refused "$code" "$err"
    fi
    case "$(printf '%s' "$resp" | jq -r '.payload.outcome // empty')" in
        deleted)
            echo "deleted $ts"
            ;;
        gone)
            echo "gone $ts: the channel has no message with that time (it was deleted already, or never posted)"
            ;;
        refused)
            code="$(printf '%s' "$resp" | jq -r '.payload.code // "?"')"
            if [ "$code" = bad_request ]; then
                _chat_refused bad_request "the request could not be sent; nothing was deleted"
            fi
            _chat_refused "$code"
            ;;
        limited)
            printf '%s' "$resp" | jq -r 'if .payload.assumed then "limited, try again later: Slack asked for a pause, and nothing was deleted"
                else "limited, wait \(.payload.retry_after_secs) s: Slack asked to wait, and nothing was deleted" end' >&2
            exit 2
            ;;
        *)
            echo "unknown: $maybe" >&2
            exit 3
            ;;
    esac
}

# $1=a session as typed: `.` for the row this session runs in, else another row's slug or id. Unpairs that row's project
# folder from its Slack channel: ONE `chat.row_pair` with a null channel, never sent again. The request carries who asks (this
# session's row and handle, checked by the daemon). Exits 0 with `unpaired <folder> from <chan> in <at>` or `unchanged: <folder>
# has no channel`; 2 when nothing was changed; 3 when it may have been (no answer, unknown).
send_chat_unpair() {
    local session="$1" row req
    _chat_identity
    row="$session"
    [ "$session" != . ] || row="$CHAT_ROW"
    _need_endpoint
    # The session as typed and the handle are free text and reach jq on stdin, as JSON strings.
    req="$({ sot_json_escape "$CHAT_ROW"; sot_json_escape "$CHAT_HANDLE"; sot_json_escape "$row"; } \
        | jq -cn 'input as $by | input as $name | input as $row
            | {v:1, id:3, kind:"req", op:"chat.row_pair", payload: {by:{row:$by, name:$name}, row:$row, channel:null}}')"
    # Past a reload under the setup lock and its wait for a start (a pairing is idempotent, so a no-answer is exit 3).
    export SEND_TIMEOUT="${TIMEOUT:-150}"
    _chat_paired "$(_chat_ask chat.row_pair "$req")" "$session"
}

# The pairing's reply $1 (empty for none) for the session $2 as typed, as `_chat_deleted` reads a delete's.
_chat_paired() {
    local resp="$1" session="$2" err code say="$2" words
    local maybe="the pairing may have been made; running the same command again is safe and says what is there now"
    [ "$session" != . ] || say="this session's project"
    if [ -z "$resp" ]; then
        echo "no answer: $maybe" >&2
        exit 3
    fi
    err="$(printf '%s' "$resp" | jq -r '.payload.error // empty')"
    if [ -n "$err" ]; then
        case "$err" in
            "unknown op"*)
                echo "refused unsupported the computer that runs your sessions runs an older Ship of Tools, which lets only a person pair or unpair a project, in the Chat drawer's channel list (Ctrl+Shift+k, Cmd+Shift+k on a Mac); nothing was changed" >&2
                exit 2 ;;
        esac
        code="$(printf '%s' "$resp" | jq -r '.payload.code // "?"')"
        # A panic in the daemon's job after the send began is an error reply too: the pairing may have been made.
        [ "$code" != handler_error ] || { echo "unknown: $maybe" >&2; exit 3; }
        case "$code" in
            stale_row)   words="no session of the computer that runs your sessions is $say, or its project folder cannot be reached (sot-fe version lists them as row <slug>)" ;;
            unmapped)    words="$say is the default session, which has no project folder" ;;
            unavailable) words="no chat is set up on the computer that runs your sessions: it is not the hub for the chat, or no Slack workspace is connected" ;;
            not_saved)   words="the chat's settings file chat.toml could not be written, or is refused as it stands; the log of Ship of Tools' service says why (sotd log-path prints where it is)" ;;
            bad_request) words="the request is not valid (this session's handle is not its row's, its row is no longer there, or the session is named in over 256 bytes)" ;;
            *)           words="$err" ;;
        esac
        _chat_refused "$code" "$words"
    fi
    case "$(printf '%s' "$resp" | jq -r '.payload.outcome // empty')" in
        unpaired)
            printf '%s' "$resp" | jq -r '.payload | "unpaired \(.folder)" + (.pairing | if . then
                " from \(if .name then "#\(.name) (\(.channel))" else .channel end) in \(if .workspace_tag then "\(.workspace_tag) (\(.workspace))" else "Slack workspace \(.workspace)" end)" else "" end)'
            ;;
        unchanged)
            printf '%s' "$resp" | jq -r '"unchanged: \(.payload.folder) has no channel"'
            ;;
        *)
            echo "unknown: $maybe" >&2
            exit 3
            ;;
    esac
}
