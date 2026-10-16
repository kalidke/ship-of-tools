#!/usr/bin/env bash
# comm-probe.sh — the per-box responder for the cross-machine comm acceptance
# matrix (comm/tests/comm-matrix.sh). Every box keeps two rows that run no
# agent, `probe-<host>` and `probe2-<host>`; the second exists only so a
# same-box send has a separate sender and receiver, which is where the Windows
# two-inbox fault lives.
#
#   comm-probe.sh up     create the two rows if missing, restart the responder
#                        in each (stop line, then start line) and wait until
#                        each has joined again
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
# A row declares its handle when its responder first joins (`serve`) and keeps
# it for good: it survives a dead responder and a daemon restart. So the handle
# alone proves nothing on a box whose rows have served before; `up` types the
# stop line, then the start line, and succeeds only once each row's registry
# `joined` differs from the one read before typing, which only a join that
# follows the start line changes. A box whose `up` failed is not measured.
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

# probe_row_id HANDLE — the workspace id of this handle's row, from the daemon's
# own list; empty when no row has it. A row declares its handle only when the
# session inside it joins (`agent_handle`, set by agent.join); until then the list
# names it by the `agent_name` it was created with, so a row whose responder never
# started is still found and `up` restarts it in place. A row that declares the
# handle wins over one that only expects it. Never the label: the label is
# cosmetic and the handle is what mail is addressed to.
probe_row_id() {
    local handle="$1" list
    list="$(probe_request '{"v":1,"id":1,"kind":"req","op":"workspace.list","payload":{}}' workspace.list)" || return 1
    printf '%s' "$list" | sot_jq -r --arg h "$handle" \
        '[(.payload.workspaces // [])[]? | select(.agent_handle == $h or ((.agent_handle // "") == "" and .agent_name == $h))]
         | sort_by(.agent_handle != $h) | .[0].workspace_id // empty' 2>/dev/null
}

# probe_joined_at HANDLE — the `joined` stamp of HANDLE's registry row, empty
# when it has none. A rejoin writes a new stamp (comm-join.sh), so a stamp that
# differs from an earlier read is a join in between.
probe_joined_at() {
    local row
    row="$(sot_registry_read "$1" 2>/dev/null)" || return 0
    printf '%s' "$row" | jq -r '.joined // empty' 2>/dev/null
    return 0
}

# probe_wait_join WSID HANDLE BEFORE — waits, bounded, until row WSID declares
# HANDLE and HANDLE's registry row was joined after BEFORE, the stamp read before
# the start line was typed: a join this responder made, not one left from an
# earlier run. A responder that never started leaves the stamp as it was, so `up`
# fails here, naming the row, rather than leaving a probe that cannot answer.
probe_wait_join() {
    local wsid="$1" handle="$2" before="$3" wait="${SOT_PROBE_READY_WAIT:-30}" list deadline now
    deadline=$(( $(date +%s) + wait ))
    while :; do
        list="$(probe_request '{"v":1,"id":1,"kind":"req","op":"workspace.list","payload":{}}' workspace.list)" || list=""
        if printf '%s' "$list" | jq -e --arg w "$wsid" --arg h "$handle" \
            'any((.payload.workspaces // [])[]?; .workspace_id == $w and .agent_handle == $h)' >/dev/null 2>&1; then
            now="$(probe_joined_at "$handle")"
            [ -n "$now" ] && [ "$now" != "$before" ] && return 0
        fi
        [ "$(date +%s)" -lt "$deadline" ] || break
        sleep 1
    done
    echo "comm-probe.sh: row $wsid did not join as @$handle within ${wait}s: its responder is not running (read the row's screen)" >&2
    return 1
}

# probe_create HANDLE — create the row, print its workspace id. `agent:"none"`
# is the bare platform shell (agents/argv.rs `agent_argv`/`none_argv`), so
# the pane is a shell we can type into (`probe_start_line`); `agent_name` is the
# handle the row expects: the capsule pins it as the responder's SOT_COMM_NAME,
# and the responder declares it by joining from inside the row (see the note in
# `serve`), never from the caller's shell.
# A same-slug create refreshes a `probe-…` row only while it is not in use; one
# in use is refused (`label_in_use`). The slug comes from the label, so this
# never adopts someone else's row.
probe_create() {
    local handle="$1" req resp wsid root
    root="$(probe_root "$handle")" || return 1
    # The root goes to the daemon inside a JSON payload, where Git Bash converts nothing: a pipe: daemon gets its own
    # spelling (sot_daemon_path).
    root="$(sot_daemon_path "$root")" || { echo "comm-probe.sh: cannot give $root in the spelling of the daemon at $ENDPOINT" >&2; return 1; }
    # The root can start with "/": it reaches jq on stdin (comm-lib-base.sh, free text and jq).
    req="$(printf '%s' "$root" | jq -Rsc --arg l "$handle" --arg an "$handle" \
        '{v:1,id:1,kind:"req",op:"workspace.create",payload:{label:$l,project_root:.,autostart_claude:false,agent:"none",agent_name:$an,task:"",boot:true}}')"
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

# probe_start_line — the line `up` types to start the responder, in the language
# of the row's shell: a login shell on Unix, cmd.exe on Windows
# (agents/argv.rs `none_argv`). cmd.exe's own `bash` is the first on its PATH,
# often WSL's bash (a System32 or WindowsApps alias), which carries none of the row's environment, so a `pipe:` daemon's
# row gets Git for Windows' launcher (`bin/bash.exe` under the root of the Git
# Bash running this, which puts Git's tools on PATH) by its Windows path. The
# script is the launcher's file argument, never `bash -c`: bash execs a lone
# `-c` command, the script's `#!/usr/bin/env bash` execs again, and the second
# exec leaves the process a Windows parent that has exited, so the row rule
# (PROTOCOL.md) cannot show it inside its row and `serve` is refused. Fails
# when there is no launcher, or a path cmd.exe would rewrite (% or ").
probe_start_line() {
    local script="$SCRIPT_DIR/comm-probe.sh" root launcher
    case "$ENDPOINT" in
        pipe:*)
            command -v cygpath > /dev/null 2>&1 && root="$(sot_native_path /)" || return 1
            launcher="${root%/}/bin/bash.exe"
            [ -f "$launcher" ] && launcher="$(cygpath -w "$launcher")" && script="$(sot_native_path "$script")" || return 1
            case "$launcher$script" in *[%\"]*) return 1 ;; esac
            printf '"%s" "%s" serve\n' "$launcher" "$script" ;;
        *)  printf "bash '%s' serve\n" "$script" ;;
    esac
}

# --- up / down / status ------------------------------------------------------

probe_up() {
    local handle wsid line before rc=0
    line="$(probe_start_line)" || probe_die "cannot start a responder in a cmd.exe row: no Git for Windows launcher" \
        "(bin/bash.exe under this Git Bash's root), or its path holds % or \""
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
        # the row having to be re-created. The stop line goes first: a live
        # responder returns on it and the start line then restarts it, so the
        # join waited for below is always this start line's; a bare shell only
        # prints "not found".
        before="$(probe_joined_at "$handle")"
        probe_type "$wsid" "$PROBE_STOP_LINE" || { rc=1; continue; }
        probe_type "$wsid" "$line" || { rc=1; continue; }
        probe_wait_join "$wsid" "$handle" "$before" && echo "@$handle has joined" || rc=1
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
    local handle wsid row rc host
    while IFS= read -r handle; do
        wsid="$(probe_row_id "$handle")" || probe_die "cannot reach this box's daemon"
        rc=0; row="$(sot_registry_read "$handle")" || rc=$?
        case "$rc" in
            0) host="$(printf '%s' "$row" | jq -r '.host // "-"' 2>/dev/null)" || host="-" ;;
            1) host="-" ;;
            *) host="unreadable" ;;
        esac
        printf '%-24s row=%-28s registry=%s\n' "@$handle" "${wsid:-<none>}" "$host"
    done < <(probe_handles)
}

# --- serve -------------------------------------------------------------------

# probe_self_handle — the handle of the row this process is running IN, asked of
# the daemon rather than guessed. $SOT_COMM_NAME wins: the capsule pins it to the
# row's `agent_name` (agents/env.rs `capsule_supervisor_env`), and the tests pin
# their own. Otherwise the row's declared `agent_handle`, or, before its first
# join (which this responder makes next), the `agent_name` it was created with;
# there is no third guess: a responder that cannot name itself must not run.
probe_self_handle() {
    if [ -n "${SOT_COMM_NAME:-}" ]; then printf '%s\n' "$SOT_COMM_NAME"; return 0; fi
    local wsid list
    wsid="$(sot_capsule_workspace_id)" || return 1
    list="$(probe_request '{"v":1,"id":1,"kind":"req","op":"workspace.list","payload":{}}' workspace.list)" || return 1
    printf '%s' "$list" | sot_jq -r --arg w "$wsid" \
        '[(.payload.workspaces // [])[]? | select(.workspace_id == $w)
          | if (.agent_handle // "") != "" then .agent_handle else .agent_name end][0] // empty' 2>/dev/null
}

# probe_inbox_stamp — a cheap size of the per-handle inbox file. Only a CHANGE
# here makes the loop spend a comm-poll.sh; equality is the common case and
# costs one stat.
probe_inbox_stamp() {
    local handle="$1" total=0 f sz
    f="$INBOX_DIR/$handle.jsonl"
    if [ -f "$f" ]; then
        sz="$(wc -c < "$f" 2>/dev/null)" || sz=0
        total=${sz:-0}
    fi
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
        # That is deliberate: a responder reading a different file from the one
        # a real session reads would test the wrong path.
        polled="$("$SCRIPT_DIR/comm-poll.sh" 2>/dev/null)" || true
        while IFS= read -r line; do
            case "$line" in *PROBE\ *) probe_handle_line "$line" poll "$me" ;; esac
        done <<< "$polled"
    done
}

# Runs only when executed, not sourced (the tests source this for the helpers).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    # A second agent inside a session (sot_require_agent) neither makes nor stops
    # rows, reads the registry nor serves as a handle; the gate comes before
    # anything else, whatever the verb (usage and an unknown verb included),
    # before the comm home is made and before any request.
    _why="$(sot_require_agent)" || probe_die "$_why"
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
