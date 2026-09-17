#!/usr/bin/env bash
# comm-wake.sh <handle> --deliver full|ping — wake a capsule-row session on
# new directed fast-comm with no harness Monitor primitive.
#
#   full — Codex sessions (ADR 0031): type EVERY new directed frame's text
#          verbatim into the row's capsule. Unchanged behaviour; this is
#          the whole of what used to be codex-watch.sh, which is now a
#          two-line shim to `--deliver full`.
#   ping — Claude sessions: the harness Monitor primitive costs a model
#          turn every ~30 minutes just to re-arm, so an idle session pays
#          for silence. This delivers ONE fixed line, never the message
#          itself: the session reads the real text with comm-poll.sh on
#          the turn the ping wakes it. A burst of N new messages still
#          costs one wake, not N (coalescing, below).
#
# Delivers into the row's capsule via `pty.input` (comm-lib.sh's
# sot_pty_input — the one live-delivery implementation, shared with
# comm-send.sh and comm-bootstrap.sh).
#
# CURSOR is IN-MEMORY ONLY, starting at the inbox's END -- a persisted one
# would replay stale backlog across a reused handle.
#
# PING MODE specifics:
#   - Filter mirrors `full`: own echoes never wake; broadcasts (to:"") wait
#     for comm-poll.sh on the next natural turn; directed frames wake.
#   - Prompt-free gate: before typing, this reads the row's current screen
#     (comm-lib.sh's sot_pty_screen) and only types when some line,
#     whitespace-trimmed, is exactly the prompt glyph `❯` -- typing into an
#     open permission dialog or menu can ANSWER it, so an unclear screen is
#     treated as "not free" and retried next cycle, cursor untouched.
#   - Coalescing: a ping already typed and not yet read (the poll cursor's
#     mtime is older than the ping) suppresses a second one -- new lines
#     just wait, since the outstanding ping already wakes the session onto
#     ALL of them. Capped at 10 minutes: if the session never polls, retry
#     rather than wait forever on one dropped ping.
#
# LIFETIME: this process ends itself when the agent (claude/codex) that
# spawned it is gone -- walks up from $PPID once at startup to find that
# process, then `kill -0`s it every cycle. This is what the old
# Monitor-only scheme couldn't do, and why idle watchers piled up as
# orphans under it.
#
# LIVENESS MARKER: the same one comm-watch.sh writes
# ($COMM_HOME/state/<handle>.watch: line 1 this process's own pid, line 2
# the arming session's id) so comm-session-start.sh's `_survived` and the
# comm-status-heartbeat.sh hook keep working unchanged. Removed on exit.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=comm-lib.sh
source "$SCRIPT_DIR/comm-lib.sh"   # sot_daemon_endpoint / sot_pty_input / sot_pty_screen / sot_capsule_workspace_id

# ---- capsule delivery: one request, no local retry ------------------------
_comm_wake_pty_input() { sot_pty_input "$@"; }    # WORKSPACE_ID DATA_B64
_comm_wake_pty_screen() { sot_pty_screen "$@"; }  # WORKSPACE_ID

# _comm_wake_pty_verdict RESP CONTEXT -> 0 advance, 1 retry (never
# recorded), 2 row gone. Never calls exit itself. Shared by `full`'s
# message delivery and `ping`'s notice delivery (same daemon contract).
_comm_wake_pty_verdict() {
    local resp="$1" ctx="$2" ok enter_sent phase code
    if [ -z "$resp" ]; then
        echo "comm-wake: capsule inject: no reply (unconfirmed), advancing $ctx" >&2
        return 0
    fi
    # One read for the whole verdict -- "|" not "\t": tab is IFS whitespace,
    # so `read` collapses consecutive tabs and silently drops the
    # (frequently empty) phase field.
    IFS='|' read -r ok enter_sent phase code <<EOF
$(printf '%s' "$resp" | jq -r '[.payload.ok // false, .payload.enter_sent // false, .payload.phase // "", .payload.code // ""] | map(tostring) | join("|")' 2>/dev/null)
EOF

    if [ "$code" = "unknown_workspace" ]; then
        echo "comm-wake: capsule row $SOT_WORKSPACE_ID is gone" >&2
        return 2
    fi

    # Permanent: retyping an oversize message would block the queue forever.
    if [ "$phase" = "size" ]; then
        echo "comm-wake: capsule inject: message too large, advancing (comm-poll can read it in full) $ctx" >&2
        return 0
    fi

    if [ "$ok" = "true" ]; then
        [ "$enter_sent" = "true" ] || echo "comm-wake: capsule inject warning: enter not sent (unconfirmed) $ctx" >&2
        return 0
    fi

    # The ONE case safe to retry: the text was never handed to the lane.
    case "$phase:$code" in
        attach:*|checkpoint:*|input:*|*:capsule_not_ready)
            echo "comm-wake: capsule inject: not delivered, retrying $ctx" >&2
            return 1
            ;;
    esac

    echo "comm-wake: capsule inject warning: outcome unknown (unconfirmed) $ctx" >&2
    return 0
}

_comm_wake_capsule_inject() {
    local from="$1" text="$2" payload b64 resp
    payload="[relay] from $from: $text"
    b64="$(printf '%s' "$payload" | base64 | tr -d '\n')"
    resp="$(_comm_wake_pty_input "$SOT_WORKSPACE_ID" "$b64")"
    _comm_wake_pty_verdict "$resp" "from $from: ${payload:0:60}"
}

# ---- ping mode: one fixed notice, never the message itself ----------------

_comm_wake_ping_inject() {
    local text="$1" b64 resp rc
    b64="$(printf '%s' "$text" | base64 | tr -d '\n')"
    resp="$(_comm_wake_pty_input "$SOT_WORKSPACE_ID" "$b64")"
    _comm_wake_pty_verdict "$resp" "ping: ${text:0:60}"
    rc=$?
    [ "$rc" -eq 0 ] && pinged_at="$(date +%s)"
    return "$rc"
}

# _comm_wake_prompt_free -> 0 iff some line of the row's CURRENT screen,
# whitespace-trimmed, is exactly the prompt glyph. A screen this can't read
# (no reply, a daemon error) reads as "not free" -- typing on an unclear
# screen is exactly the risk this gate exists to avoid.
_comm_wake_prompt_free() {
    local resp
    resp="$(_comm_wake_pty_screen "$SOT_WORKSPACE_ID" 2>/dev/null)" || return 1
    [ -n "$resp" ] || return 1
    printf '%s' "$resp" | jq -e '
        (.payload.lines // []) | any(gsub("^[ \t]+|[ \t]+$";"") == "❯")
    ' >/dev/null 2>&1
}

# _comm_wake_ping_outstanding -> 0 iff a ping was accepted and the session's
# own poll cursor has not moved since (still unread) -- new lines wait for
# it rather than piling on a second ping. A 10-minute cap forces a retry if
# the session never polls (a dropped ping must not wedge delivery forever).
_comm_wake_ping_outstanding() {
    [ "${pinged_at:-0}" -gt 0 ] || return 1
    local now; now=$(date +%s)
    [ $((now - pinged_at)) -lt 600 ] || return 1
    local mtime
    mtime="$(stat -c %Y "$CURSOR_FILE" 2>/dev/null || stat -f %m "$CURSOR_FILE" 2>/dev/null || echo 0)"
    [ "$mtime" -lt "$pinged_at" ]
}

# ---- lifetime: end with the agent that spawned this, never orphan --------

# _comm_wake_find_agent_pid -> the pid of the nearest ancestor whose comm is
# `claude` or `codex`, walking up from $PPID (stop at pid 1 -> none found,
# printed nothing, rc 1). No owner found means no liveness tie -- this
# process then runs for as long as its capsule leg does (today's codex-watch
# behaviour, unchanged when nothing claims it).
_comm_wake_find_agent_pid() {
    local pid="${PPID:-}" comm ppid
    while [ -n "$pid" ] && [ "$pid" != "1" ]; do
        comm="$(ps -o comm= -p "$pid" 2>/dev/null | tr -d ' ')"
        case "$comm" in
            claude|codex) printf '%s\n' "$pid"; return 0 ;;
        esac
        ppid="$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')"
        [ -n "$ppid" ] && [ "$ppid" != "$pid" ] || break
        pid="$ppid"
    done
    return 1
}

# _comm_wake_owner_alive -> 0 when no owner is known (never trigger exit) or
# when the known owner still answers `kill -0`.
_comm_wake_owner_alive() {
    [ -n "${AGENT_PID:-}" ] || return 0
    kill -0 "$AGENT_PID" 2>/dev/null
}

# ---- the two delivery bodies, one poll loop --------------------------------

_comm_wake_deliver_full() {
    delivered_through="$pos"
    local lineno=0 from to text rc
    while IFS= read -r line; do
        lineno=$((lineno + 1))
        from=$(printf '%s' "$line" | jq -r '.from // ""' 2>/dev/null)
        to=$(printf '%s' "$line" | jq -r 'if has("to") then .to else "__legacy__" end' 2>/dev/null)
        text=$(printf '%s' "$line" | jq -r '.text // .message // .msg // ""' 2>/dev/null)
        if [ "$from" = "$HANDLE" ] || [ "$to" = "" ] || [ -z "$text" ]; then
            delivered_through=$((pos + lineno))
            continue
        fi
        _comm_wake_capsule_inject "$from" "$text"
        rc=$?
        if [ "$rc" -eq 2 ]; then exit 0; fi
        [ "$rc" -eq 0 ] || break
        delivered_through=$((pos + lineno))
    done < <(sed -n "$((pos + 1)),${total}p" "$INBOX")
    pos="$delivered_through"
}

_comm_wake_deliver_ping() {
    local any_directed=0 all_selftest=1 from to text rc text_to_type

    while IFS= read -r line; do
        from=$(printf '%s' "$line" | jq -r '.from // ""' 2>/dev/null)
        to=$(printf '%s' "$line" | jq -r 'if has("to") then .to else "__legacy__" end' 2>/dev/null)
        text=$(printf '%s' "$line" | jq -r '.text // .message // .msg // ""' 2>/dev/null)
        [ "$from" = "$HANDLE" ] && continue
        [ "$to" = "" ] && continue
        [ -z "$text" ] && continue
        any_directed=1
        [ "$from" = "__selftest__" ] || all_selftest=0
    done < <(sed -n "$((pos + 1)),${total}p" "$INBOX")

    if [ "$any_directed" -eq 0 ]; then
        pos="$total"
        return
    fi
    _comm_wake_ping_outstanding && return    # already awake for these; wait for the read
    _comm_wake_prompt_free || return         # dialog/menu/draft on screen; retry next cycle

    if [ "$all_selftest" -eq 1 ]; then
        text_to_type="$SELFTEST_TEXT"
    else
        text_to_type="$PING_TEXT"
    fi
    _comm_wake_ping_inject "$text_to_type"
    rc=$?
    if [ "$rc" -eq 2 ]; then exit 0; fi
    [ "$rc" -eq 0 ] && pos="$total"
}

_comm_wake_run() {
    pos=$(wc -l < "$INBOX" 2>/dev/null || echo 0)
    pinged_at=0

    # No pane-liveness check beyond the agent-owner one above: a capsule
    # leg's own process group reaps this when the row itself goes away.
    while :; do
        _comm_wake_owner_alive || exit 0
        sleep 2
        _comm_wake_bound_log
        [ -f "$INBOX" ] || continue
        total=$(wc -l < "$INBOX" 2>/dev/null || echo 0)
        if [ "$total" -lt "$pos" ]; then pos=0; fi   # inbox rotated/truncated
        [ "$total" -gt "$pos" ] || continue
        if [ "$DELIVER" = "full" ]; then
            _comm_wake_deliver_full
        else
            _comm_wake_deliver_ping
        fi
    done
}

# Keeps LOG_FILE at roughly LOG_CAP bytes, rewritten in place (keeps O_APPEND working).
LOG_CAP=262144
_comm_wake_bound_log() {
    local size tmp
    [ -f "$LOG_FILE" ] || return 0
    size=$(wc -c < "$LOG_FILE" 2>/dev/null || echo 0)
    [ "$size" -gt "$LOG_CAP" ] || return 0
    tmp="$LOG_FILE.bound.$$"
    tail -c "$LOG_CAP" "$LOG_FILE" > "$tmp" 2>/dev/null || { rm -f "$tmp"; return 0; }
    cat "$tmp" > "$LOG_FILE" 2>/dev/null
    rm -f "$tmp"
}

_comm_wake_cleanup() { rm -f "${MARKER:-}" 2>/dev/null || true; }

_comm_wake_main() {
    HANDLE="${1:?usage: comm-wake.sh <handle> --deliver full|ping}"
    DELIVER="full"
    if [ "${2:-}" = "--deliver" ]; then DELIVER="${3:-full}"; fi
    case "$DELIVER" in
        full|ping) ;;
        *) echo "comm-wake: --deliver must be full or ping" >&2; exit 2 ;;
    esac

    # Checked ONCE here, not inside a command substitution (silent
    # forever-advance on an empty reply) -- same discipline as $SOT_WORKSPACE_ID.
    if ! SOT_WORKSPACE_ID="$(sot_capsule_workspace_id)"; then
        echo "comm-wake: not a capsule row (no \$SOT_WORKSPACE_ID and no derivable \$SOT_COMM_SELF_FILE) -- the caller falls back to a harness Monitor" >&2
        exit 3
    fi
    export SOT_WORKSPACE_ID
    ENDPOINT="$(sot_daemon_endpoint)" \
        || { echo "ERROR: could not resolve the daemon endpoint for capsule delivery" >&2; exit 1; }

    COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
    INBOX="$COMM_HOME/inbox/$HANDLE.jsonl"
    CURSOR_FILE="$COMM_HOME/read/$HANDLE.cursor"
    STATE_DIR="$COMM_HOME/state"; mkdir -p "$STATE_DIR"
    LOG_FILE="$STATE_DIR/comm-wake-$HANDLE.log"
    MARKER="$STATE_DIR/$HANDLE.watch"
    _comm_wake_bound_log
    # Own diagnostics go to a durable, size-bounded log, never /dev/null.
    exec 2>>"$LOG_FILE"

    if [ -n "${SOT_COMM_HOME:-}" ]; then
        PING_TEXT="[sot-comm] new message for @$HANDLE — run $SOT_COMM_HOME/bin/comm-poll.sh"
    else
        PING_TEXT="[sot-comm] new message for @$HANDLE — run ~/.sot-comm/bin/comm-poll.sh"
    fi
    SELFTEST_TEXT="[sot-comm] wake selftest OK — nothing to read"

    AGENT_PID=""
    AGENT_PID="$(_comm_wake_find_agent_pid || true)"

    # Same marker comm-watch.sh writes: line 1 this process's own pid
    # (liveness), line 2 the session that armed it (identity).
    printf '%s\n%s\n' "$$" "${CLAUDE_CODE_SESSION_ID:-}" > "$MARKER" 2>/dev/null || true
    trap _comm_wake_cleanup EXIT

    _comm_wake_run
}

# Runs only when executed, not sourced (tests source this for the helpers).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    _comm_wake_main "$@"
fi
