#!/usr/bin/env bash
# sot-capsule-capable: 1
# codex-watch.sh <handle> [<tmux-pane>] — wake for CODEX sessions (ADR 0031).
#
# Codex has no harness-Monitor primitive, so an idle codex session cannot be
# woken by an inbox write alone. This daemon POLLS the handle's inbox (~2s;
# NFS — inotify silently misses writes there, hence poll, same reason the CC
# Monitor polls) and delivers each new directed frame into the session.
#
# A pane arg means tmux mode; no pane arg means capsule mode (the caller
# already decided that), delivering via `pty.input`.
# Filter mirrors comm-watch.sh: own echoes never inject; broadcasts (to:"")
# file silently for comm-poll on the next natural turn; directed frames and
# legacy no-`to` lines inject. Selftest frames DO inject (they prove this
# exact path).
#
# Cursor is IN-MEMORY ONLY in both modes, starting at the inbox's END --
# a persisted one replays stale backlog across a reused handle.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=comm-lib.sh
source "$SCRIPT_DIR/comm-lib.sh"   # _sot_secure_dir / sot_daemon_endpoint / sot_oneshot_request

# sot_tmux_socket — tmux mode only; unlike comm-lib.sh's own version,
# verifies each candidate against the TARGET PANE.
sot_tmux_socket() {
    local uid sock sotd_bin
    local -a candidates=()
    uid="$(id -u)"

    [ -n "${SOT_TMUX_SOCK:-}" ] && candidates+=("$SOT_TMUX_SOCK")
    [ -n "${TMUX:-}" ] && candidates+=("${TMUX%%,*}")

    sotd_bin="$(command -v sotd 2>/dev/null || true)"
    if [ -n "$sotd_bin" ]; then
        sock="$("$sotd_bin" tmux-socket-path 2>/dev/null || true)"
        [ -n "$sock" ] && candidates+=("$sock")
    fi

    if [ -n "${XDG_RUNTIME_DIR:-}" ] && [ ! -L "$XDG_RUNTIME_DIR" ] && [ -d "$XDG_RUNTIME_DIR" ]; then
        local xowner xmode
        xowner="$(stat -c '%u' "$XDG_RUNTIME_DIR" 2>/dev/null || true)"
        xmode="$(stat -c '%a' "$XDG_RUNTIME_DIR" 2>/dev/null || true)"
        if [ -n "$xowner" ] && [ "$xowner" = "$uid" ] \
           && [ -n "$xmode" ] && [ $((0$xmode & 0077)) -eq 0 ]; then
            candidates+=("$XDG_RUNTIME_DIR/sot/tmux.sock")
        fi
    fi
    [ -d "/run/user/$uid" ] && candidates+=("/run/user/$uid/sot/tmux.sock")
    candidates+=("/tmp/sot-$uid/tmux.sock")
    candidates+=("/tmp/tmux-$uid/default")

    local seen=""
    for sock in "${candidates[@]}"; do
        [ -n "$sock" ] || continue
        case " $seen " in
            *" $sock "*) continue ;;
        esac
        seen="$seen $sock"
        _sot_secure_dir "$(dirname "$sock")" || continue
        [ -S "$sock" ] || continue
        if tmux -S "$sock" display-message -t "$PANE" -p '#{pane_id}' >/dev/null 2>&1; then
            printf '%s\n' "$sock"
            return 0
        fi
    done

    echo "sot_tmux_socket: pane $PANE was not found on candidate tmux sockets:$seen" >&2
    return 1
}

# ---- capsule-mode delivery: one request, no local retry -------------------
_codex_watch_pty_input() {  # WORKSPACE_ID DATA_B64
    local wsid="$1" data="$2" frame
    # base64 can begin with "/" (MSYS2 path conversion): --rawfile, never --arg.
    local _data_file; _data_file="$(sot_jq_rawfile "$data")" || return 1
    frame="$(jq -nc --arg w "$wsid" --rawfile d "$_data_file" \
        '{v:1,id:1,kind:"req",op:"pty.input",payload:{workspace_id:$w,data_b64:$d,enter:true}}')"
    local rc=$?
    rm -f "$_data_file"
    [ "$rc" -eq 0 ] || return 1
    # ~18s is the daemon's own worst case for one enter=true write.
    SOT_SEND_TIMEOUT="${SOT_SEND_TIMEOUT:-20}" sot_oneshot_request "$frame" "pty.input"
}

# _codex_watch_capsule_inject FROM TEXT -> 0 advance, 1 retry (text never
# recorded), 2 row gone. Never calls exit itself.
_codex_watch_capsule_inject() {
    local from="$1" text="$2" payload b64 resp ok enter_sent phase code
    payload="[relay] from $from: $text"
    b64="$(printf '%s' "$payload" | base64 | tr -d '\n')"
    resp="$(_codex_watch_pty_input "$SOT_WORKSPACE_ID" "$b64")"

    if [ -z "$resp" ]; then
        echo "codex-watch: capsule inject: no reply (unconfirmed), advancing from $from: ${payload:0:60}" >&2
        return 0
    fi

    # One read for the whole verdict -- ok/enter_sent/phase/code, each
    # defaulted so a malformed or partial reply parses to safe values.
    # "|" not "\t": tab is IFS whitespace, so `read` collapses consecutive
    # tabs and silently drops the (frequently empty) phase field.
    IFS='|' read -r ok enter_sent phase code <<EOF
$(printf '%s' "$resp" | jq -r '[.payload.ok // false, .payload.enter_sent // false, .payload.phase // "", .payload.code // ""] | map(tostring) | join("|")' 2>/dev/null)
EOF

    if [ "$code" = "unknown_workspace" ]; then
        echo "codex-watch: capsule row $SOT_WORKSPACE_ID is gone" >&2
        return 2
    fi

    # Permanent: retyping an oversize message would block the queue forever.
    if [ "$phase" = "size" ]; then
        echo "codex-watch: capsule inject: message too large, advancing (comm-poll can read it in full) from $from: ${payload:0:60}" >&2
        return 0
    fi

    if [ "$ok" = "true" ]; then
        if [ "$enter_sent" != "true" ]; then
            echo "codex-watch: capsule inject warning: enter not sent (unconfirmed) from $from: ${payload:0:60}" >&2
        fi
        return 0
    fi

    # The ONE case safe to retry: the text was never handed to the lane.
    case "$phase:$code" in
        attach:*|checkpoint:*|input:*|*:capsule_not_ready)
            echo "codex-watch: capsule inject: not delivered, retrying from $from: ${payload:0:60}" >&2
            return 1
            ;;
    esac

    echo "codex-watch: capsule inject warning: outcome unknown (unconfirmed) from $from: ${payload:0:60}" >&2
    return 0
}

# _codex_watch_deliver_tmux FROM TEXT -> always 0 (fire-and-forget; the
# only confirmation tmux offers is the keystrokes landing at all).
_codex_watch_deliver_tmux() {
    local from="$1" text="$2"
    # -l: literal keystrokes (no key-name interpretation); Enter sent
    # separately so codex submits the injected line as a turn.
    tmux -S "$SOT_TMUX_SOCK" send-keys -t "$PANE" -l "[relay] from $from: $text" 2>/dev/null
    # The Codex TUI treats a keystroke burst as a paste and swallows an
    # Enter that arrives in the same burst as a newline, leaving the frame
    # unsubmitted at the prompt (field report 2026-09-07: immediate Enter
    # hung every frame; a 0.5 s pause submitted every one).
    sleep 0.5
    tmux -S "$SOT_TMUX_SOCK" send-keys -t "$PANE" Enter 2>/dev/null
    return 0
}

# One loop for both modes: mode is decided ONCE before it (pane arg set or
# not), and the per-line delivery is the only branch inside -- tmux mode's
# _deliver always returns 0 (advance), capsule's carries the 0/1/2 contract.
_codex_watch_run() {
    if [ -n "$PANE" ]; then
        SOT_TMUX_SOCK="$(sot_tmux_socket)" \
            || { echo "ERROR: could not resolve a secure tmux socket for pane $PANE — see reason above" >&2; exit 1; }
    else
        # Checked ONCE here, not inside a command substitution (silent forever-advance on empty reply).
        : "${SOT_WORKSPACE_ID:?codex-watch: capsule mode needs SOT_WORKSPACE_ID}"
        ENDPOINT="$(sot_daemon_endpoint)" \
            || { echo "ERROR: could not resolve the daemon endpoint for capsule delivery" >&2; exit 1; }
    fi

    pos=$(wc -l < "$INBOX" 2>/dev/null || echo 0)

    # No pane-liveness check in capsule mode: that process is reaped with
    # the capsule leg's own process group.
    while :; do
        sleep 2
        _codex_watch_bound_log
        if [ -n "$PANE" ]; then
            # Pane gone → session over → exit.
            tmux -S "$SOT_TMUX_SOCK" display-message -t "$PANE" -p '#{pane_id}' >/dev/null 2>&1 || exit 0
        fi
        [ -f "$INBOX" ] || continue
        total=$(wc -l < "$INBOX" 2>/dev/null || echo 0)
        if [ "$total" -lt "$pos" ]; then pos=0; fi   # inbox rotated/truncated
        [ "$total" -gt "$pos" ] || continue
        # `delivered_through` advances only past a line actually handed
        # off (or correctly skipped) — a failed delivery stops the batch
        # there, retried next cycle. sed bounds the read to EXACTLY
        # [pos+1, total]: a concurrent append past `total` waits for the
        # NEXT cycle, never delivered twice.
        delivered_through="$pos"
        lineno=0
        while IFS= read -r line; do
            lineno=$((lineno + 1))
            from=$(printf '%s' "$line" | jq -r '.from // ""' 2>/dev/null)
            to=$(printf '%s' "$line" | jq -r 'if has("to") then .to else "__legacy__" end' 2>/dev/null)
            text=$(printf '%s' "$line" | jq -r '.text // .message // .msg // ""' 2>/dev/null)
            if [ "$from" = "$HANDLE" ] || [ "$to" = "" ] || [ -z "$text" ]; then
                delivered_through=$((pos + lineno))
                continue
            fi
            if [ -n "$PANE" ]; then
                _codex_watch_deliver_tmux "$from" "$text"
                rc=$?
            else
                _codex_watch_capsule_inject "$from" "$text"
                rc=$?
            fi
            if [ "$rc" -eq 2 ]; then
                exit 0
            fi
            [ "$rc" -eq 0 ] || break
            delivered_through=$((pos + lineno))
        done < <(sed -n "$((pos + 1)),${total}p" "$INBOX")
        pos="$delivered_through"
    done
}

# Keeps LOG_FILE at roughly LOG_CAP bytes, rewritten in place (keeps O_APPEND working).
LOG_CAP=262144
_codex_watch_bound_log() {
    local size tmp
    [ -f "$LOG_FILE" ] || return 0
    size=$(wc -c < "$LOG_FILE" 2>/dev/null || echo 0)
    [ "$size" -gt "$LOG_CAP" ] || return 0
    tmp="$LOG_FILE.bound.$$"
    tail -c "$LOG_CAP" "$LOG_FILE" > "$tmp" 2>/dev/null || { rm -f "$tmp"; return 0; }
    cat "$tmp" > "$LOG_FILE" 2>/dev/null
    rm -f "$tmp"
}

_codex_watch_main() {
    HANDLE="${1:?usage: codex-watch.sh <handle> [<tmux-pane>]}"
    PANE="${2:-}"
    COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
    INBOX="$COMM_HOME/inbox/$HANDLE.jsonl"
    STATE_DIR="$COMM_HOME/state"; mkdir -p "$STATE_DIR"
    LOG_FILE="$STATE_DIR/codex-watch-$HANDLE.log"
    _codex_watch_bound_log
    # Own diagnostics go to a durable, size-bounded log, never /dev/null.
    exec 2>>"$LOG_FILE"

    _codex_watch_run
}

# Runs only when executed, not sourced (tests source this for the helpers).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    _codex_watch_main "$@"
fi
