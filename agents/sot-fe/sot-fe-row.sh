# sot-fe's verbs on a sibling row: type into it, read its screen, move it to another account.
# Sourced by sot-fe; defines functions only.

# Portable base64 encode with NO embedded newlines. GNU coreutils' `base64`
# has `-w0` (also what git-bash on Windows ships); BSD/macOS `base64` (and
# some minimal/busybox builds) lack it and wrap at 76 columns by default —
# `tr -d '\n'` strips that wrap uniformly instead of maintaining a second
# encode path.
sot_base64_encode() {
    if base64 --help 2>&1 | grep -q -- ' -w'; then
        base64 -w0
    else
        base64 | tr -d '\n'
    fi
}

# ADR 0042 amendment (2026-09-07): a session types into a SIBLING row.
# $1=workspace (id/label/slug), $2=text (may be empty — a bare --enter is
# legal), $3=enter ("true"|"false"). Answered (op pty.input) — unlike
# pty.write (this connection's own pty, fire-and-forget), a caller with no
# pane to look at needs the outcome. Exits 2 on a daemon-reported error.
send_pty_input() {
    local ws="$1" text="$2" enter="$3" ws_id data_b64 origin_for_use payload req resp
    _need_endpoint
    ws_id="$(_ws_id_or_passthrough "$ws")"
    export SEND_TIMEOUT="${TIMEOUT:-10}"
    data_b64="$(printf '%s' "$text" | sot_base64_encode)"
    origin_for_use="$ORIGIN"
    if [ -z "$origin_for_use" ]; then
        # Default: the caller's own comm handle, when derivable — else the
        # literal "sot-fe". Isolated in its own subshell (never `eval`d
        # into THIS shell) so a comm-context.sh failure can't clobber any
        # variable here; `2>/dev/null || true` means "no handle" is exactly
        # as safe as "comm-context.sh doesn't exist at all."
        origin_for_use="$( (eval "$("$SCRIPT_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null; printf '%s' "${NAME:-}") 2>/dev/null || true)"
        [ -n "$origin_for_use" ] || origin_for_use="sot-fe"
    fi
    # MSYS2 argv-conversion guard: base64 can BEGIN with '/', which git-bash
    # would rewrite into a Windows path under --arg — so the payload goes
    # through sot_jq_rawfile like origin below.
    local _data_file; _data_file="$(sot_jq_rawfile "$data_b64")" || exit 1
    payload="$(jq -nc --arg w "$ws_id" --rawfile d "$_data_file" --argjson e "$enter" \
        '{workspace_id:$w, data_b64:$d, enter:$e}')"
    # MSYS2 argv-conversion guard (comm-lib.sh's sot_jq_rawfile): origin is
    # free text (a comm handle or a user-set --origin) and must never reach
    # jq via --arg.
    local _origin_file; _origin_file="$(sot_jq_rawfile "$origin_for_use")" || exit 1
    payload="$(printf '%s' "$payload" | jq -c --rawfile o "$_origin_file" '. + {origin:$o}')"
    rm -f "${_origin_file:?}"
    req="$(jq -nc --argjson p "$payload" '{v:1, id:2, kind:"req", op:"pty.input", payload:$p}')"
    resp="$(sot_send "$req" pty.input || true)"
    if [ -z "$resp" ]; then
        echo "ERROR: no response from pty.input within ${SEND_TIMEOUT}s via $ENDPOINT." >&2
        exit 2
    fi
    local err code
    err="$(printf '%s' "$resp" | jq -r '.payload.error // empty')"
    if [ -n "$err" ]; then
        code="$(printf '%s' "$resp" | jq -r '.payload.code // "?"')"
        echo "ERROR: $err [$code]" >&2
        exit 2
    fi
    printf '%s' "$resp" | jq -r '.payload | "ok bytes=\(.bytes) runtime=\(.runtime) enter=\(.enter // "unknown")"'
}

# ADR 0042 amendment (2026-09-07): the CURRENT screen of a named row — no
# scrollback, no history (op pty.screen). $1=workspace (id/label/slug).
# Exits 2 on a daemon-reported error.
send_pty_screen() {
    local ws="$1" ws_id resp
    _need_endpoint
    ws_id="$(_ws_id_or_passthrough "$ws")"
    export SEND_TIMEOUT="${TIMEOUT:-10}"
    # One implementation: comm-lib.sh's sot_pty_screen.
    resp="$(sot_pty_screen "$ws_id" || true)"
    if [ -z "$resp" ]; then
        echo "ERROR: no response from pty.screen within ${SEND_TIMEOUT}s via $ENDPOINT." >&2
        exit 2
    fi
    local err code
    err="$(printf '%s' "$resp" | jq -r '.payload.error // empty')"
    if [ -n "$err" ]; then
        code="$(printf '%s' "$resp" | jq -r '.payload.code // "?"')"
        echo "ERROR: $err [$code]" >&2
        exit 2
    fi
    printf '%s' "$resp" | jq -r '.payload.lines[]'
    printf '%s' "$resp" | jq -r '.payload |
        "cols=\(.cols) rows=\(.rows) cursor=" +
        (if .cursor then "\(.cursor.row),\(.cursor.col)" else "none" end) +
        " runtime=\(.runtime)"' >&2
}

# ADR 0046 decision 6: move a live capsule row to another account and resume
# the same conversation there. One request/response op (workspace.reauth).
# The row comes from THIS session's own SOT_WORKSPACE_ID, the same way the
# resume id comes from its own CLAUDE_CODE_SESSION_ID -- the caller is the row
# being moved, and nothing else knows which conversation it is; an
# absent one is refused here rather than silently becoming "the most recent
# conversation" on a login that has no such record. The daemon writes its
# accept BEFORE ending the leg, so a printed `ok` is honest even though this
# very process is about to be replaced; anything after it belongs to the new
# session, not this one.
send_workspace_reauth() {
    local account="$1" resp
    if [ -z "${SOT_WORKSPACE_ID:-}" ]; then
        echo "ERROR: SOT_WORKSPACE_ID is unset, so this is not a capsule row and there is no row to move. Nothing was sent." >&2
        exit 2
    fi
    local ws_id="$SOT_WORKSPACE_ID"
    local resume="${CLAUDE_CODE_SESSION_ID:-}"
    if [ -z "$resume" ]; then
        echo "ERROR: CLAUDE_CODE_SESSION_ID is unset, so there is no transcript id to resume. Run this from inside the claude session being moved -- a reauth has no 'most recent conversation' to fall back to on another login." >&2
        exit 2
    fi
    # Validated here, before it is bound, for the same reason comm-spawn.sh
    # validates its own --account: it is what lets this bind the allowlisted
    # `acc` name rather than a fresh one (see the jq --arg allowlist test's
    # own doc). It also turns a typo into an immediate, clear refusal instead
    # of a round trip that comes back "no such account".
    case "$account" in
        [a-z0-9]*) : ;;
        *) echo "ERROR: '$account' is not an account name (expected ^[a-z0-9][a-z0-9_-]*$)." >&2; exit 2 ;;
    esac
    case "$account" in
        *[!a-z0-9_-]*) echo "ERROR: '$account' is not an account name (expected ^[a-z0-9][a-z0-9_-]*$)." >&2; exit 2 ;;
    esac
    _need_endpoint
    export SEND_TIMEOUT="${TIMEOUT:-20}"
    local payload req
    # `acc` and `id` are the allowlisted names for a validated account name
    # and an id; neither value can begin with "/" (the MSYS2 argv-conversion
    # hazard those names exist to bound), so neither needs --rawfile.
    payload="$(jq -nc --arg w "$ws_id" --arg acc "$account" --arg id "$resume" \
        '{workspace_id:$w, account:$acc, resume:$id}')"
    req="$(jq -nc --argjson p "$payload" '{v:1, id:2, kind:"req", op:"workspace.reauth", payload:$p}')"
    resp="$(sot_send "$req" workspace.reauth || true)"
    if [ -z "$resp" ]; then
        echo "ERROR: no response from workspace.reauth within ${SEND_TIMEOUT}s via $ENDPOINT. The accept is written BEFORE anything is torn down, so no reply means the switch did NOT happen -- check the row's phase before retrying." >&2
        exit 1
    fi
    local err code
    err="$(printf '%s' "$resp" | jq -r '.payload.error // empty')"
    if [ -n "$err" ]; then
        code="$(printf '%s' "$resp" | jq -r '.payload.code // "?"')"
        echo "ERROR: $err [$code]" >&2
        # Discovery belongs to the daemon: print what IT can see rather than
        # guessing at folders from here, and never create one -- an empty
        # account folder is a valid account with NO login, which turns a
        # reauth into a login prompt with the conversation stranded behind it.
        printf '%s' "$resp" | jq -r '.payload.accounts // [] | if length > 0 then "  accounts this daemon can see: " + join(", ") else empty end' >&2
        exit 2
    fi
    printf '%s' "$resp" | jq -r '"ok \(.payload.code) account=\(.payload.account) workspace=\(.payload.workspace_id)"'
    echo "note: this session's leg is being replaced now -- everything in flight dies with it, and the resumed session starts on /sot-session-start." >&2
}
