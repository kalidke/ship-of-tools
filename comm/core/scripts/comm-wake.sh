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
# TWO INBOXES ON WINDOWS, one cursor each, exactly as comm-watch.sh already
# reads them: there is no relay bridge on a frontend box, so the frontend
# files every inbound frame into its own fe-inbox.jsonl, while a send from a
# session on the SAME box still lands in inbox/<handle>.jsonl. Watching only
# the per-handle file there woke a session on half its mail and never on the
# half that comes from another box -- which is why a Windows session fell
# back to the harness Monitor instead of this watcher. sot_fe_inbox_path
# (comm-lib.sh) is the ONE place that platform branch lives: off Windows it
# prints nothing and this watcher has a single source, exactly as before.
#
# PING MODE specifics:
#   - Filter mirrors `full`: own echoes never wake; broadcasts (to:"") wait
#     for comm-poll.sh on the next natural turn; directed frames wake.
#   - Prompt-free gate: before typing, this reads the row's current screen
#     (comm-lib.sh's sot_pty_screen) and only types when the CURSOR sits at
#     the start of an input line marked by `❯` -- a grey prompt suggestion
#     is byte-identical to a typed draft in the text, and only the cursor
#     separates them; typing into an open permission dialog or menu can
#     ANSWER it, so an unclear screen is treated as "not free" and retried
#     next cycle, cursor untouched.
#   - Coalescing: a ping already typed and not yet read (the poll cursor's
#     mtime is older than the ping) suppresses a second one -- new lines
#     just wait, since the outstanding ping already wakes the session onto
#     ALL of them. Capped at 10 minutes: if the session never polls, retry
#     rather than wait forever on one dropped ping. Separately, once the
#     cursor DOES move: its content is the newest-read message's ts
#     (comm-poll.sh), compared against the newest pending line's ts -- a
#     cursor that already covers the pending batch (read through some other
#     real poll) advances past it with no second ping, rather than treating
#     "cursor moved at all" as reason enough to ping again.
#
# LIFETIME: this process ends itself when the agent (claude/codex) that
# spawned it is gone -- the caller passes that pid with `--owner <pid>`
# (found while the caller itself was still attached, a better vantage than
# this script has once backgrounded), then `kill -0`s it every cycle. This
# is what the old Monitor-only scheme couldn't do, and why idle watchers
# piled up as orphans under it.
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
_comm_wake_row() { sot_wake_row "$@"; }            # HANDLE

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
$(printf '%s' "$resp" | sot_jq -r '[.payload.ok // false, .payload.enter_sent // false, .payload.phase // "", .payload.code // ""] | map(tostring) | join("|")' 2>/dev/null)
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
    return $?
}

# _comm_wake_prompt_free -> 0 free (the cursor sits at the start of the row's
# input line), 1 screen read but NOT free (dialog/menu/draft on screen --
# today's fail-closed retry, cursor untouched), 2 NO REPLY at all (transport
# error, empty response -- distinct from 1 so the caller can count these
# separately and give up on a daemon that never answers instead of retrying
# it forever), 3 the daemon does not have this row at all. 3 is not a busy
# row: it is no row. Collapsed into 1 it read as a dialog that never cleared,
# so the ping was held forever while this watcher kept reporting healthy.
_comm_wake_prompt_free() {
    local resp
    resp="$(_comm_wake_pty_screen "$SOT_WORKSPACE_ID" 2>/dev/null)"
    [ -n "$resp" ] || return 2
    sot_row_gone "$resp" && return 3
    sot_prompt_free "$resp" && return 0
    return 1
}

# _comm_wake_retarget -> 0 with $SOT_WORKSPACE_ID repointed at the row that
# declares $HANDLE right now, or 1 meaning do nothing this cycle. Exits the
# watcher outright when the handle cannot be aimed at exactly one row.
#
# The id this watcher started with was frozen into its environment at spawn
# (comm-session-start.sh) and a session that continues in another row keeps
# waking the row it used to be in. There is NO fallback to that frozen id: a
# daemon that cannot answer workspace.list would not have answered pty.screen
# either, so the fallback buys two seconds and re-arms the defect.
#
# $no_reply_count is _comm_wake_run's, shared rather than duplicated so an
# unanswerable daemon gives up on the SAME 5 strikes the screen probe uses.
# Every exit leaves the marker to the EXIT trap, which removes it only if this
# process owns it -- a blind extra call would drop someone else's.
_comm_wake_retarget() {
    local row rc=0
    row="$(_comm_wake_row "$HANDLE")" || rc=$?
    case "$rc" in
        0) SOT_WORKSPACE_ID="$row"; return 0 ;;
        1) echo "comm-wake: no live row declares @$HANDLE — exiting so the next session start re-arms" >&2; exit 0 ;;
        3) echo "comm-wake: two or more rows declare @$HANDLE — refusing to guess which to wake; exiting so the next session start re-arms" >&2; exit 0 ;;
        *)
            no_reply_count=$((no_reply_count + 1))
            if [ "$no_reply_count" -ge 5 ]; then
                echo "comm-wake: workspace.list unanswered 5 times; exiting so session start can fall back to the Monitor" >&2
                exit 0
            fi
            return 1
            ;;
    esac
}

# ---- lifetime: end with the agent that spawned this, never orphan --------

# The owning agent's pid is no longer discovered here: the caller (typically
# comm-session-start.sh) is still directly attached to the real originating
# claude/codex process at the moment it decides to spawn this watcher, and
# passes it with `--owner <pid>`. Finding it AFTER this script is already
# backgrounded (the old _comm_wake_find_agent_pid, which walked $PPID up
# looking for a claude/codex `comm`) was strictly worse vantage for the same
# answer. No `--owner` given means no liveness tie -- this process then runs
# for as long as its capsule leg does (today's codex-watch behaviour,
# unchanged when nothing claims it).

# _comm_wake_owner_alive -> 0 when no owner is known (never trigger exit) or
# when the known owner still answers `kill -0`.
_comm_wake_owner_alive() {
    [ -n "${AGENT_PID:-}" ] || return 0
    kill -0 "$AGENT_PID" 2>/dev/null
}

# ---- one source at a time -------------------------------------------------
#
# $INBOX, $SELECT and $pos name the source the loop below is currently reading;
# everything downstream stays single-file and single-schema. The ADMISSION RULE
# travels with the source (the $SELECTS array beside $SOURCES, and $CURSORS for
# the read cursor) exactly as comm-watch.sh carries a filter per source, so
# there is no kind tag to keep in step with the file list and no per-file branch
# anywhere below.

# _comm_wake_admit LINE SELECT -- print LINE's sender and return 0 when this
# source's rule admits it; return 1 (printing nothing) when it does not.
#
# The `+` prefix is load-bearing: it separates "jq emitted nothing" (not
# admitted) from "admitted, and the sender is the empty string", which the pair
# of shell tests this replaced could tell apart and this must too.
_comm_wake_admit() {
    local out
    out="$(printf '%s' "$1" | sot_jq -r --arg me "$HANDLE" "$2 | \"+\" + (.from // \"\")" 2>/dev/null)"
    [ -n "$out" ] || return 1
    printf '%s' "${out#+}"
}

# _comm_wake_advance -- move every source's in-memory cursor ($POS) to the end
# of the batch this cycle just scanned ($ENDS). Called only where the cycle is
# finished with that batch: nothing announced, or the notice typed. A cycle
# that held the ping (a busy prompt, an unanswered daemon) advances nothing,
# so the next cycle re-scans the same lines and announces them then.
_comm_wake_advance() {
    local i
    for i in "${!SOURCES[@]}"; do POS[$i]="${ENDS[$i]}"; done
}

# ---- the two delivery bodies, one poll loop --------------------------------

_comm_wake_deliver_full() {
    # Once per BATCH, never per line: `full` injects one message at a time, so
    # resolving inside the loop would be one workspace.list per message.
    _comm_wake_retarget || return
    delivered_through="$pos"
    local lineno=0 from text rc
    while IFS= read -r line; do
        lineno=$((lineno + 1))
        if ! from="$(_comm_wake_admit "$line" "$SELECT")"; then
            delivered_through=$((pos + lineno))
            continue
        fi
        text=$(printf '%s' "$line" | jq -r '.text // .message // .msg // ""' 2>/dev/null)
        _comm_wake_capsule_inject "$from" "$text"
        rc=$?
        if [ "$rc" -eq 2 ]; then exit 0; fi
        [ "$rc" -eq 0 ] || break
        delivered_through=$((pos + lineno))
    done < <(sed -n "$((pos + 1)),${total}p" "$INBOX")
    pos="$delivered_through"
}

# ONE CYCLE, not one source. This body reads EVERY inbox, then decides once:
# the ping is a notice that mail exists, so a cross-box frame and a same-box
# frame arriving in the same 2s cycle are one wake, exactly as a burst within
# one file always was. Running the body per source typed the same line twice
# for one batch of mail, re-resolved the row once per source, and halved the
# give-up budget below (`no_reply_count` and `blocked_since` count CYCLES, and
# a per-source body counted them twice a cycle).
_comm_wake_deliver_ping() {
    local any_directed=0 all_selftest=1 from rc text_to_type read_pos
    local i total line
    ENDS=()

    for i in "${!SOURCES[@]}"; do
        INBOX="${SOURCES[$i]}"
        # Where this source's batch ENDS. Set even when nothing admissible is
        # found, so a cycle that announces nothing still advances past what it
        # read and a broadcast-only batch is not re-scanned forever.
        ENDS[$i]="${POS[$i]}"
        [ -r "$INBOX" ] || continue
        total="$(sot_file_lines "$INBOX")"
        # inbox rotated/truncated: re-read the (now smaller) file from line 1.
        if [ "$total" -lt "${POS[$i]}" ]; then POS[$i]=0; ENDS[$i]=0; fi
        [ "$total" -gt "${POS[$i]}" ] || continue
        ENDS[$i]="$total"
        # ALREADY-READ check, against THIS source's own cursor -- never one
        # shared, since the two inboxes have unrelated line counts and a shared
        # cursor would silence whichever file is shorter. The cursor is the
        # NUMBER of lines the session has been shown (comm-lib.sh's
        # sot_cursor_offset / sot_fe_cursor_offset, which migrate a legacy ts
        # cursor as they read). A cursor that already
        # reaches the end of this batch means the session read it through a
        # real poll -- advance past it with no ping. Comparing timestamps here
        # could not separate two frames filed in the same second, so a second
        # frame could be skipped as "already read" and never announced at all.
        #
        # This is the ONLY suppression left (messaging ruling §4): the old
        # `_comm_wake_ping_outstanding` also withheld a ping while an earlier
        # one sat unread, which turned one stalled session into permanent
        # deafness for every message behind it. A genuinely NEW line now always
        # pings again, and a missed ping is harmless -- the recipient's own Stop
        # hook reads the inbox at its next turn boundary.
        read_pos="$("${CURSORS[$i]}" "$HANDLE")"
        [ "$read_pos" -ge "$total" ] && continue
        while IFS= read -r line; do
            from="$(_comm_wake_admit "$line" "${SELECTS[$i]}")" || continue
            any_directed=1
            [ "$from" = "__selftest__" ] || all_selftest=0
        done < <(sed -n "$((POS[$i] + 1)),${total}p" "$INBOX")
    done

    if [ "$any_directed" -eq 0 ]; then
        _comm_wake_advance
        return
    fi

    _comm_wake_retarget || return
    _comm_wake_prompt_free
    local pf_rc=$?
    if [ "$pf_rc" -eq 3 ]; then
        # The gate's own dead-row detection, finally reachable: the
        # `unknown_workspace` check in _comm_wake_pty_verdict sits DOWNSTREAM
        # of this gate.
        echo "comm-wake: capsule row $SOT_WORKSPACE_ID is gone" >&2
        exit 0
    fi
    if [ "$pf_rc" -eq 2 ]; then
        no_reply_count=$((no_reply_count + 1))
        if [ "$no_reply_count" -ge 5 ]; then
            echo "comm-wake: pty.screen unanswered 5 times; exiting so session start can fall back to the Monitor" >&2
            # Owner-checked, like the EXIT path — a blind rm here would drop
            # someone else's marker just as readily.
            _comm_wake_cleanup
            exit 0
        fi
        return   # no reply this cycle; retry, cursor untouched
    fi
    no_reply_count=0
    if [ "$pf_rc" -ne 0 ]; then
        # Honesty of the record: a gate that can suppress delivery
        # indefinitely must leave a trace of having done so. One line per
        # TRANSITION, never per cycle — this loop runs every 2s.
        if [ -z "$blocked_since" ]; then
            blocked_since="$(now_iso)"
            echo "comm-wake: prompt not free; ping held since $blocked_since" >&2
        fi
        return   # dialog/menu/draft on screen; retry next cycle
    fi
    if [ -n "$blocked_since" ]; then
        # The fact is the GATE's transition, not a delivery: the inject below
        # can still fail, and saying "sent" before attempting it records a
        # delivery that never happened — with blocked_since already cleared, no
        # later cycle says otherwise. The inject reports its own outcome.
        echo "comm-wake: prompt free again after being held since $blocked_since" >&2
        blocked_since=""
    fi

    if [ "$all_selftest" -eq 1 ]; then
        text_to_type="$SELFTEST_TEXT"
    else
        text_to_type="$PING_TEXT"
    fi
    _comm_wake_ping_inject "$text_to_type"
    rc=$?
    if [ "$rc" -eq 2 ]; then exit 0; fi
    [ "$rc" -eq 0 ] && _comm_wake_advance
}

_comm_wake_run() {
    local i
    POS=()
    for i in "${!SOURCES[@]}"; do POS+=("$(sot_file_lines "${SOURCES[$i]}")"); done
    no_reply_count=0
    blocked_since=""

    # No pane-liveness check beyond the agent-owner one above: a capsule
    # leg's own process group reaps this when the row itself goes away.
    while :; do
        _comm_wake_owner_alive || exit 0
        sleep 2
        _comm_wake_bound_log
        if [ "$DELIVER" = "ping" ]; then
            # One notice for the whole cycle, whichever inboxes it came from:
            # this body reads every source itself.
            _comm_wake_deliver_ping
            continue
        fi
        # `full` types each frame's own text, so it is per-line by definition
        # and a second source is simply a second batch of lines.
        for i in "${!SOURCES[@]}"; do
            INBOX="${SOURCES[$i]}"
            SELECT="${SELECTS[$i]}"
            pos="${POS[$i]}"
            [ -f "$INBOX" ] || continue
            total=$(sot_file_lines "$INBOX")
            if [ "$total" -lt "$pos" ]; then pos=0; fi   # inbox rotated/truncated
            [ "$total" -gt "$pos" ] && _comm_wake_deliver_full
            # Whatever the body consumed, never what it was handed: a body
            # that stops early (a refused inject) leaves $pos on the last
            # line it actually delivered, and that is what this source
            # resumes from next cycle.
            POS[$i]="$pos"
        done
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

# Delete ONLY a marker this process still owns. A blind `rm` was the second
# half of the leak the exclusive claim below fixes: after a lost race the
# DEPARTING watcher removed the WINNER's marker, leaving a live watcher with
# nothing recording it and a third one free to start on top. Line 1 is the
# owning pid, so ownership is a fact this can check rather than assume.
_comm_wake_cleanup() {
    [ -n "${MARKER:-}" ] || return 0
    [ "$(sed -n '1p' "$MARKER" 2>/dev/null)" = "$$" ] || return 0
    rm -f "$MARKER" 2>/dev/null || true
}

# Claim the marker ATOMICALLY: `noclobber` makes `>` fail when the file already
# exists, and an exclusive create is atomic on this home (NFSv4.2 — the same
# property that let the old flock workarounds be retired). The claim IS the
# mutual exclusion; nothing else needs to be.
_comm_wake_claim() {
    ( set -o noclobber; printf '%s\n%s\n' "$$" "${CLAUDE_CODE_SESSION_ID:-}" > "$MARKER" ) 2>/dev/null
}

_comm_wake_main() {
    HANDLE="${1:?usage: comm-wake.sh <handle> --deliver full|ping [--owner <pid>]}"
    shift
    DELIVER="full"
    OWNER_PID=""
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --deliver) DELIVER="${2:-full}"; shift 2 ;;
            --owner)   OWNER_PID="${2:-}"; shift 2 ;;
            *) shift ;;
        esac
    done
    case "$DELIVER" in
        full|ping) ;;
        *) echo "comm-wake: --deliver must be full or ping" >&2; exit 2 ;;
    esac

    # THE OWNER, for every leg. A watcher with no owner cannot end with the
    # agent it wakes: it outlives the session, keeps typing into a row that has
    # moved on, and its marker makes the next bootstrap report SURVIVED (the
    # immortal watcher). `--owner <pid>` is an OVERRIDE for a caller that is
    # still directly attached to the agent and has the better vantage;
    # otherwise the owner is DISCOVERED from this process's own ancestry
    # (comm-lib.sh's sot_owner_pid). Discovery is what lets the refusal apply to
    # every leg, Codex's `--deliver full` included: no caller has to remember a
    # flag, so no leg can be left ownerless.
    AGENT_PID=""
    if [[ "$OWNER_PID" =~ ^[0-9]+$ ]]; then
        AGENT_PID="$OWNER_PID"
    else
        AGENT_PID="$(sot_owner_pid || true)"
    fi
    if ! [[ "$AGENT_PID" =~ ^[0-9]+$ ]]; then
        echo "comm-wake: no owning claude/codex ancestor found and no --owner given — refusing to start an ownerless watcher" >&2
        exit 2
    fi

    # Checked ONCE here, not inside a command substitution (silent
    # forever-advance on an empty reply) -- same discipline as $SOT_WORKSPACE_ID.
    # What this id decides is whether this is a capsule row AT ALL, which the
    # environment really does answer. It is no longer a wake TARGET: every
    # batch re-resolves the row from the daemon (_comm_wake_retarget).
    if ! SOT_WORKSPACE_ID="$(sot_capsule_workspace_id)"; then
        echo "comm-wake: this shell's identity names no row (its \$SOT_COMM_SELF_FILE basename is not <host>__<row-id>.txt, and there is no \$SOT_WORKSPACE_ID to fall back on) -- the caller falls back to a harness Monitor" >&2
        exit 3
    fi
    export SOT_WORKSPACE_ID
    ENDPOINT="$(sot_daemon_endpoint)" \
        || { echo "ERROR: could not resolve the daemon endpoint for capsule delivery" >&2; exit 1; }

    COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
    # Every inbox this handle receives into, each with the rule that admits a
    # line from it and the cursor that says how much of it the session has been
    # shown. The two selects are the two comm-watch.sh applies to the same two
    # files: the per-handle inbox holds frames filed FOR this handle, so any
    # DIRECTED frame admits (a line with no `.to` key predates the stamp and is
    # treated as directed; a broadcast's empty `.to` waits for comm-poll.sh on
    # the next natural turn), while the frontend inbox is ONE file per box
    # shared by every handle on it, so there `.to` must be ours exactly. Both
    # also require some text: a frame with none wakes nobody. The frontend
    # inbox exists on Windows only, and only where a frontend files into it.
    SOURCES=("$COMM_HOME/inbox/$HANDLE.jsonl")
    SELECTS=('select(.from != $me and ((.to // "?") != "") and ((.text // .message // .msg // "") != ""))')
    CURSORS=(sot_cursor_offset)
    _fe_inbox="$(sot_fe_inbox_path)"
    if [ -n "$_fe_inbox" ]; then
        SOURCES+=("$_fe_inbox")
        SELECTS+=('select(.from != $me and ((.to // "") == $me) and ((.text // .message // .msg // "") != ""))')
        CURSORS+=(sot_fe_cursor_offset)
    fi
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

    # START-TIME MUTEX (messaging ruling §3). The marker is the one thing that
    # says a watcher for this handle is alive, so it is also what refuses a
    # second one: two watchers would double every ping and the one that lost
    # the marker could never be reaped by it. This replaces the `pgrep` dedupe
    # comm-session-start.sh used to do from the outside.
    # Verified by IDENTITY, not by `kill -0` alone (sot_watcher_pid_for): the
    # marker outlives reboots on a shared home, so a reused pid would otherwise
    # refuse a legitimate watcher forever — and codex-watch.sh writes this same
    # marker, so the wrong answer here is exactly the shape this ruling exists
    # to remove: deaf, and reporting healthy. An unverifiable marker is stale.
    # The claim comes FIRST and the liveness check only runs when it loses.
    # What this replaces was a test followed by a write with nothing between
    # them: two starts could both pass the liveness check before either wrote,
    # and the loser was then invisible to the very marker meant to reap it —
    # an immortal watcher, doubling every ping for the life of the box. The
    # comment there called it a start-time mutex; a test-then-write is not one.
    #
    # Same marker comm-watch.sh writes: line 1 this process's own pid
    # (liveness), line 2 the session that armed it (identity).
    if ! _comm_wake_claim; then
        # Something holds the marker. If it is a real watcher, refuse.
        if _live="$(sot_watcher_pid_for "$HANDLE")"; then
            echo "comm-wake: a watcher for @$HANDLE is already live (pid $_live in $MARKER) — refusing to start a second" >&2
            exit 4
        fi
        # Unverifiable, so stale — a reboot on a shared home, or a watcher
        # killed before its cleanup ran. Remove it and re-claim through the
        # same atomic door: if THAT loses, a concurrent starter got there
        # first and it is the live one, so refuse rather than overwrite. Two
        # starters can never both come away believing they own the marker.
        rm -f "$MARKER" 2>/dev/null || true
        if ! _comm_wake_claim; then
            echo "comm-wake: lost the marker race for @$HANDLE to a concurrent start — refusing to start a second" >&2
            exit 4
        fi
    fi
    trap _comm_wake_cleanup EXIT

    _comm_wake_run
}

# Runs only when executed, not sourced (tests source this for the helpers).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    _comm_wake_main "$@"
fi
