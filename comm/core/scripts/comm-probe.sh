#!/usr/bin/env bash
# comm-probe.sh — the per-box responder for the cross-machine comm acceptance
# matrix (comm/core/tests/comm-matrix.sh). Every box keeps two rows that run no
# agent, `probe-<host>` and `probe2-<host>`; the second exists only so a
# same-box send has a separate sender and receiver, which is where the Windows
# two-inbox fault lives.
#
#   comm-probe.sh up     create the two rows if missing and start the responder
#                        in each (idempotent — a row that already serves ignores
#                        the typed line)
#   comm-probe.sh down    stop the responders, leaving the rows
#   comm-probe.sh serve   the responder itself: the FOREGROUND process of a
#                         probe row. Not a daemon, not a watcher.
#   comm-probe.sh status  print the two handles, their row ids and whether the
#                         daemon names them
#
# WHY IT CANNOT LEAK LIKE THE OLD WATCHERS. It is not another watcher: it is
# the foreground process of a visible row. It starts no background child, and
# its cursor lives only in memory. Stopping the row (ConPTY job containment on
# Windows, the capsule on Linux) ends it.
#
# THE PROBE-ONLY RULE, enforced in three places below. This script creates,
# types into and replies to nothing whose handle does not begin `probe`: `up`
# refuses to adopt a row it did not name, `serve` refuses to run under any
# other handle, and every reply target is checked before a send. So the
# responder can never relay mail to a real session, and `down` can never stop
# one.
#
# A row whose responder is NOT running still declares its handle (the daemon
# records `agent_name` at workspace.create, handlers.rs `from_label`), so the
# frontend's receipt — which comes from declared rows, gpu.rs `receipt_for` —
# stays honest and the matrix's delivery lines keep meaning something when the
# echo is missing. That separation is the whole instrument.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=comm-lib.sh
source "$SCRIPT_DIR/comm-lib.sh"   # sot_host / sot_daemon_endpoint / sot_pty_input / sot_oneshot_request

PROBE_READ_TIMEOUT="${SOT_PROBE_READ_TIMEOUT:-2}"   # seconds blocked in `read`
PROBE_STOP_LINE="PROBE-STOP"

# A handle this script may touch. The single predicate behind the probe-only
# rule — every caller below funnels through it rather than re-testing the
# prefix, so there is one place to read and one place to get it wrong.
probe_handle_ok() {
    case "${1:-}" in probe*) return 0 ;; *) return 1 ;; esac
}

probe_die() { echo "comm-probe.sh: $*" >&2; exit 1; }

# probe_request FRAME OP — one daemon request on this box's own endpoint.
# `sot_oneshot_request` and `sot_pty_input` both read the GLOBAL $ENDPOINT, and
# it is resolved once in the dispatch at the bottom rather than here: every
# lookup below runs in a command substitution, so an assignment made here dies
# with that subshell and the next un-substituted call (`probe_type`) finds
# $ENDPOINT unset — which under `set -u` is a hard error, not a fallback.
probe_request() {
    local frame="$1" op="$2"
    sot_oneshot_request "$frame" "$op"
}

# probe_root HANDLE — this probe row's own project root. Each row needs a
# DISTINCT one: the daemon refuses a second workspace at a root another already
# holds (`duplicate_root`), so both probe rows at $HOME is a create that fails
# on the second. A directory per handle, named for the handle (which carries the
# host), so the shared comm home stays collision-free across boxes. A probe row
# holds no user work, and this is where that is true on disk too.
probe_root() {
    local dir="$COMM_HOME/probe/$1"
    mkdir -p "$dir" 2>/dev/null || return 1
    printf '%s\n' "$dir"
}

# probe_row_id HANDLE — the workspace id of the row DECLARING this handle, from
# the daemon's own list. Empty (rc 1) when no row declares it. Matches on the
# declared handle, never on the label: the label is cosmetic and the handle is
# what mail is addressed to.
probe_row_id() {
    local handle="$1" list
    list="$(probe_request '{"v":1,"id":1,"kind":"req","op":"workspace.list","payload":{}}' workspace.list)" || return 1
    printf '%s' "$list" | sot_jq -r --arg h "$handle" \
        '[(.payload.workspaces // [])[]? | select(.agent_handle == $h) | .workspace_id][0] // empty' 2>/dev/null
}

# probe_create HANDLE — create the row, print its workspace id. `agent:"none"`
# is the bare platform shell (capsule_workspace.rs `agent_argv`/`none_argv`), so
# the pane is a login shell we can type into; `agent_name` is what declares the
# handle to the daemon at creation, which is why this path needs no agent.join
# from the caller's shell (see the note in `serve`).
# A same-slug create is an id-preserving REFRESH in the daemon, not an error
# (`Workspaces::insert`), and the slug comes from the label — so this can only
# ever refresh a row already labelled `probe-…`, never adopt someone else's.
probe_create() {
    local handle="$1" req resp wsid root
    root="$(probe_root "$handle")" || return 1
    # The root goes through `sot_jq_rawfile`, never `--arg`: on git-bash with a
    # native jq, MSYS2 rewrites any argv element that starts with "/" into a
    # Windows path, and a project root is one of the three values in this tree
    # that legitimately does.
    local rootfile; rootfile="$(sot_jq_rawfile "$root")" || return 1
    req="$(jq -nc --arg l "$handle" --rawfile p "$rootfile" --arg an "$handle" \
        '{v:1,id:1,kind:"req",op:"workspace.create",payload:{label:$l,project_root:$p,autostart_claude:false,agent:"none",agent_name:$an,task:"",boot:true}}')"
    rm -f "$rootfile"
    resp="$(probe_request "$req" workspace.create)" || return 1
    wsid="$(printf '%s' "$resp" | sot_jq -r '.payload.workspace_id // empty' 2>/dev/null)"
    [ -n "$wsid" ] || {
        echo "comm-probe.sh: workspace.create for @$handle failed: $(printf '%s' "$resp" | jq -c '.payload' 2>/dev/null || echo 'no reply')" >&2
        return 1
    }
    printf '%s\n' "$wsid"
}

# probe_type WSID TEXT — one line typed into the row, Enter appended. The same
# one live-delivery implementation every other comm script uses; UNGATED,
# because a probe row's pane is a bare shell or this responder's `read`, never a
# permission dialog that a keystroke could answer.
# WAITS for the row: a just-created capsule answers `capsule_not_ready (phase:
# starting)` for the first seconds of its life, and a create followed straight
# by a type is exactly that race. Bounded, and the LAST failure is reported
# verbatim rather than a summary — `ok:true` is the daemon's own success shape,
# so an absent `error` key is not what is checked (an `// empty` test over a
# missing key produces no output, which `jq -e` reports as failure: that read a
# successful type as a failed one).
probe_type() {
    local wsid="$1" text="$2" resp b64 deadline
    b64="$(printf '%s' "$text" | base64 | tr -d '\n')"
    deadline=$(( $(date +%s) + ${SOT_PROBE_READY_WAIT:-30} ))
    while :; do
        resp="$(sot_pty_input "$wsid" "$b64")" || resp=""
        printf '%s' "$resp" | jq -e '.payload.ok == true' >/dev/null 2>&1 && return 0
        printf '%s' "$resp" | jq -e '.payload.code == "capsule_not_ready"' >/dev/null 2>&1 || break
        [ "$(date +%s)" -lt "$deadline" ] || break
        sleep 1
    done
    echo "comm-probe.sh: pty.input into row $wsid failed: $(printf '%s' "$resp" | jq -c '.payload' 2>/dev/null || echo 'no reply')" >&2
    return 1
}

probe_handles() {
    local host; host="$(sot_host)" || return 1
    printf 'probe-%s\nprobe2-%s\n' "$host" "$host"
}

# --- up / down / status ------------------------------------------------------

probe_up() {
    local handle wsid rc=0
    while IFS= read -r handle; do
        probe_handle_ok "$handle" || probe_die "refusing to create a row named '$handle' — probe rows only"
        wsid="$(probe_row_id "$handle")" || probe_die "cannot reach this box's daemon"
        if [ -z "$wsid" ]; then
            wsid="$(probe_create "$handle")" || { rc=1; continue; }
            echo "created @$handle -> $wsid"
        else
            echo "reusing @$handle -> $wsid"
        fi
        # No `exec`: a responder that exits leaves the row's own shell alive, so
        # a later `up` restarts it in place instead of the row's pane ending and
        # the row having to be re-created. A responder that IS running reads this
        # line, finds no PROBE in it and ignores it, which is what makes `up`
        # idempotent.
        probe_type "$wsid" "bash '$SCRIPT_DIR/comm-probe.sh' serve" || rc=1
    done < <(probe_handles)
    return $rc
}

probe_down() {
    local handle wsid rc=0
    while IFS= read -r handle; do
        probe_handle_ok "$handle" || probe_die "refusing to stop a row named '$handle' — probe rows only"
        wsid="$(probe_row_id "$handle")" || probe_die "cannot reach this box's daemon"
        [ -n "$wsid" ] || { echo "no row declares @$handle"; continue; }
        # The stop travels the SAME typed path a probe does, so it needs no
        # signal, no pidfile and no process lookup: a live responder reads it and
        # returns; a dead one leaves a harmless "command not found" in a shell.
        probe_type "$wsid" "$PROBE_STOP_LINE" || rc=1
        echo "stopped @$handle ($wsid)"
    done < <(probe_handles)
    return $rc
}

probe_status() {
    local handle wsid
    while IFS= read -r handle; do
        wsid="$(probe_row_id "$handle")" || probe_die "cannot reach this box's daemon"
        printf '%-24s row=%-28s registry=%s\n' "@$handle" "${wsid:-<none>}" \
            "$(jq -r --arg n "$handle" '(.agents[$n].host // "-")' "$REGISTRY" 2>/dev/null || echo '-')"
    done < <(probe_handles)
}

# --- serve -------------------------------------------------------------------

# probe_self_handle — the handle of the row this process is running IN, asked of
# the daemon rather than guessed. $SOT_COMM_NAME wins when the caller pinned one
# (the tests do); otherwise the row's declared `agent_handle` is the answer, and
# there is no third guess: a responder that cannot name itself must not run.
probe_self_handle() {
    if [ -n "${SOT_COMM_NAME:-}" ]; then printf '%s\n' "$SOT_COMM_NAME"; return 0; fi
    local wsid list
    wsid="$(sot_capsule_workspace_id)" || return 1
    list="$(probe_request '{"v":1,"id":1,"kind":"req","op":"workspace.list","payload":{}}' workspace.list)" || return 1
    printf '%s' "$list" | sot_jq -r --arg w "$wsid" \
        '[(.payload.workspaces // [])[]? | select(.workspace_id == $w) | .agent_handle][0] // empty' 2>/dev/null
}

# probe_inbox_stamp — a cheap total over both inbox files (the per-handle one and,
# on Windows, the frontend's shared fe-inbox). Only a CHANGE here makes the loop
# spend a comm-poll.sh; equality is the common case and costs one stat.
probe_inbox_stamp() {
    local handle="$1" total=0 f sz
    for f in "$INBOX_DIR/$handle.jsonl" "$(sot_fe_inbox_path)"; do
        [ -n "$f" ] && [ -f "$f" ] || continue
        sz="$(wc -c < "$f" 2>/dev/null)" || sz=0
        total=$((total + ${sz:-0}))
    done
    printf '%s\n' "$total"
}

# probe_field LINE KEY — the @handle following KEY in a PROBE line, without its
# '@'. Empty when absent.
probe_field() {
    printf '%s\n' "$1" | sed -n "s/.*[[:space:]]$2[[:space:]]*@\([A-Za-z0-9._-]*\).*/\1/p" | head -n 1
}

# probe_reply NONCE REPLY HOP WOKE — the whole of what a PROBE does. One send
# (the echo, or the hop), then one VERDICT to the original asker carrying this
# box's OWN sender rc and its literal first line: that line is how the matrix
# sees the reverse direction, which no sender on the other end can report.
probe_reply() {
    local nonce="$1" reply="$2" hop="$3" woke="$4" me="$5"
    local target payload out rc=0 first
    if [ -n "$hop" ]; then
        target="$hop"; payload="PROBE $nonce reply @$reply"
    else
        target="$reply"; payload="ECHO $nonce from @$me woke:$woke"
    fi
    probe_handle_ok "$target" || { echo "comm-probe.sh: refusing to reply to @$target" >&2; return 1; }
    out="$("$SCRIPT_DIR/comm-relay.sh" send "@$target" "$payload" 2>&1)" || rc=$?
    first="$(printf '%s\n' "$out" | sed -n '1p')"
    printf '%s -> @%s rc=%s %s\n' "$payload" "$target" "$rc" "$first"
    "$SCRIPT_DIR/comm-relay.sh" send "@$reply" \
        "VERDICT $nonce @$me->@$target rc=$rc $first" >/dev/null 2>&1 || true
    return 0
}

# probe_handle_line LINE WOKE ME — act on one line of text, from the pane or
# from the inbox. Acts ONLY on a well-formed PROBE: a 12-hex nonce and a probe
# reply handle. Anything else is ignored in silence, which is what lets `up`
# type a start line into a row that is already serving.
probe_handle_line() {
    local line="$1" woke="$2" me="$3" probe nonce reply hop
    probe="$(printf '%s\n' "$line" | grep -o 'PROBE [0-9a-f]\{12\}.*' | head -n 1)" || return 0
    [ -n "$probe" ] || return 0
    nonce="$(printf '%s\n' "$probe" | awk '{print $2}')"
    reply="$(probe_field "$probe" reply)"
    hop="$(probe_field "$probe" hop)"
    [ -n "$reply" ] || return 0
    probe_handle_ok "$reply" || { echo "ignored: reply @$reply is not a probe handle" >&2; return 0; }
    [ -z "$hop" ] || probe_handle_ok "$hop" || { echo "ignored: hop @$hop is not a probe handle" >&2; return 0; }
    probe_reply "$nonce" "$reply" "$hop" "$woke" "$me"
}

probe_serve() {
    local me line rc stamp last_stamp polled
    me="$(probe_self_handle)" || probe_die "cannot resolve this row's handle — run \`up\` from a session on this box first"
    probe_handle_ok "$me" || probe_die "refusing to serve as '@$me' — probe rows only"
    export SOT_COMM_NAME="$me"
    # The join happens HERE, inside the row, and never in `up`: comm-join.sh
    # declares to the daemon the row THIS SHELL'S IDENTITY names
    # (sot_capsule_workspace_id) and keys the identity slot by it, so a join run
    # from the CALLER's shell would re-declare the caller's own live row under a
    # probe handle. Inside the row, that identity is provably this row's own.
    "$SCRIPT_DIR/comm-join.sh" --name "$me" >/dev/null 2>&1 \
        || echo "comm-probe.sh: WARNING — join as @$me failed; local sends may not name this row" >&2
    echo "serving as @$me (${PROBE_READ_TIMEOUT}s read, $PROBE_STOP_LINE to stop)"
    last_stamp="$(probe_inbox_stamp "$me")"
    while :; do
        printf '❯ '
        line=""; rc=0
        read -r -t "$PROBE_READ_TIMEOUT" line || rc=$?
        printf '\n'
        if [ "$rc" -ne 0 ] && [ "$rc" -lt 128 ] && [ -z "$line" ]; then
            # A closed stdin (EOF, rc 1) is the row going away, not a timeout
            # (rc > 128). Returning is how the process ends with the pane.
            [ "$rc" -eq 1 ] && { echo "stdin closed — stopping"; return 0; }
        fi
        case "$line" in
            "$PROBE_STOP_LINE") echo "stopped"; return 0 ;;
            *PROBE\ *) probe_handle_line "$line" typed "$me" ;;
        esac
        stamp="$(probe_inbox_stamp "$me")"
        [ "$stamp" = "$last_stamp" ] && continue
        last_stamp="$stamp"
        # comm-poll.sh, the reader a session uses — not a bespoke inbox read.
        # That is deliberate: on Windows it is the ONLY thing that knows about
        # the frontend's shared fe-inbox, and a responder reading a different
        # file from the one a real session reads would test the wrong path.
        polled="$("$SCRIPT_DIR/comm-poll.sh" 2>/dev/null)" || true
        while IFS= read -r line; do
            case "$line" in *PROBE\ *) probe_handle_line "$line" poll "$me" ;; esac
        done <<< "$polled"
    done
}

# Runs only when executed, not sourced (the tests source this for the helpers).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    ensure_home
    ENDPOINT="$(sot_daemon_endpoint)" || probe_die "no sotd daemon found on this box"
    case "${1:-}" in
        up)     probe_up ;;
        down)   probe_down ;;
        serve)  probe_serve ;;
        status) probe_status ;;
        *)      echo "usage: comm-probe.sh up|down|serve|status" >&2; exit 1 ;;
    esac
fi
