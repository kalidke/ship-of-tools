# sot-fe's reply in the row's channel: `chat post`, one `chat.post` of kind `row` into the current thread.
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
            busy)        words="the chat service is busy" ;;
            bad_request) words="the request is not valid (empty or over-long text, or a name that is not this row's)" ;;
            failed)      words="the chat request failed" ;;
        esac
    fi
    echo "refused $code${words:+ $words}" >&2
    exit 2
}

# One request, one reply, no second try: the chat's post is submitted once (chat guarantee 12), so unlike
# `send_fe_command` nothing here sends a frame again. $1=op, $2=the request frame.
_chat_ask() {
    sot_send "$2" "$1" || true
}

# $1=text, $2=thread ts or "". Posts for the row this session runs in ($SOT_WORKSPACE_ID) under its comm handle, into
# the thread, and exits 0 with `posted <ts>`; 2 with a refusal, a limit, or an outcome that says the post may exist.
send_chat_post() {
    local text="$1" thread="$2" row="${SOT_WORKSPACE_ID:-}" handle res req resp err state
    if [ -z "$row" ]; then
        echo "refused: SOT_WORKSPACE_ID is unset, so this is not a row and has no channel. Nothing was sent." >&2
        exit 2
    fi
    handle="$(_chat_handle)"
    if [ -z "$handle" ]; then
        echo "refused: this session has no comm handle (not joined, or its handle is another row's). Nothing was sent." >&2
        exit 2
    fi
    _need_endpoint
    export SEND_TIMEOUT="${TIMEOUT:-10}"
    # The row is id-shaped (a workspace id).
    req="$(jq -nc --arg w "$row" '{v:1, id:2, kind:"req", op:"chat.resolve", payload:{row:$w}}')"
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
        unmapped) _chat_refused unmapped "this row's folder maps to no channel" ;;
        *) _chat_refused invalid "the chat's configuration does not map this row" ;;
    esac
    res="$(printf '%s' "$resp" | jq -c '.payload')"
    # The text, the handle and the thread are free text and reach jq on stdin, as JSON strings after the resolved
    # pair (comm-lib-base.sh, free text and jq); the row is the id above.
    req="$({ printf '%s\n' "$res"; sot_json_escape "$row"; sot_json_escape "$handle"; sot_json_escape "$thread"; sot_json_escape "$text"; } \
        | jq -cn 'input as $r | input as $row | input as $name | input as $thread | input as $text
            | {v:1, id:3, kind:"req", op:"chat.post",
               payload: ({target:{row:$row, workspace:$r.workspace, channel:$r.channel}, kind:"row", name:$name, text:$text}
                         + (if $thread == "" then {} else {thread:$thread} end))}')"
    export SEND_TIMEOUT="${TIMEOUT:-40}"
    resp="$(_chat_ask chat.post "$req")"
    if [ -z "$resp" ]; then
        echo "no answer: the post may exist; do not send it again" >&2
        exit 2
    fi
    err="$(printf '%s' "$resp" | jq -r '.payload.error // empty')"
    if [ -n "$err" ]; then
        _chat_refused "$(printf '%s' "$resp" | jq -r '.payload.code // "?"')" "$err"
    fi
    case "$(printf '%s' "$resp" | jq -r '.payload.outcome // empty')" in
        posted)
            printf '%s' "$resp" | jq -r '"posted \(.payload.ts)"'
            ;;
        refused)
            _chat_refused "$(printf '%s' "$resp" | jq -r '.payload.code // "?"')"
            ;;
        limited)
            printf '%s' "$resp" | jq -r '"limited, wait \(.payload.retry_after_secs) s"' >&2
            exit 2
            ;;
        *)
            echo "unknown: the post may exist; do not send it again" >&2
            exit 2
            ;;
    esac
}
