#!/usr/bin/env bash
# comm-session-start.sh — the deterministic sot-comm receive-bootstrap, split
# into TWO phases (Codex review finding 5) so a message can never land before
# the Monitor that would wake on it exists:
#
#   comm-session-start.sh             phase 1 ("arm"): resolve identity, print
#                                      ONE line and STOP —
#                                      either SURVIVED (nothing to do) or
#                                      BOOTSTRAP-ARM (arm the printed MONITOR
#                                      command, THEN run phase 2).
#   comm-session-start.sh --catch-up  phase 2, run only after the Monitor is
#                                      armed: poll the backlog and the sot
#                                      layer. Prints the final verdict.
#   comm-session-start.sh --context   read-only: print a short context block
#                                      if survived, else say so and name the
#                                      phase-1 re-run. NEVER joins,
#                                      polls, or writes anything to disk
#                                      (comm-context.sh honors
#                                      $SOT_COMM_READONLY for both of its own
#                                      writes — ensure_home and the legacy
#                                      self-file self-heal).
#
# IDENTITY PRECEDENCE (Codex review findings 1–3): pin ($SOT_COMM_NAME, or a
# private $SOT_COMM_SELF_FILE whose file already exists and validates) →
# validated self-file NAME → fresh derivation. This script NEVER passes an
# explicit `--name` to comm-join.sh — comm-join.sh's OWN precedence (--name
# arg > $SOT_COMM_NAME env > self-file NAME > derive) already implements the
# same order correctly; manufacturing an explicit --name here from a
# lower-priority source (the bug this PR shipped with) can override a real
# launcher pin. A subagent/lane that does not own the ambient pane-keyed
# self-file MUST pin a distinct $SOT_COMM_NAME and, ideally, its own private
# $SOT_COMM_SELF_FILE — see references/reclaim-handle.md. When neither is
# given and the self-file already names a DIFFERENT, validated identity, this
# script REFUSES to join at all rather than silently stealing that slot (this
# is exactly how PR1's own coordinator session was clobbered by an unpinned
# subagent during testing — see the implementation report).
#
# See comm/adapters/claude/sot-session-start/SKILL.md for what the calling
# skill does with each printed line.
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=comm-lib.sh
source "$SCRIPT_DIR/comm-lib.sh"

MODE="arm"
case "${1:-}" in
    --context)  MODE="context" ;;
    --catch-up) MODE="catchup" ;;
esac

_watch_marker() { printf '%s/state/%s.watch\n' "${SOT_COMM_HOME:-$HOME/.sot-comm}" "$1"; }

# The owner-pid walk lives in comm-lib.sh (`sot_owner_pid`) now: comm-wake.sh
# discovers its own owner with it, so no caller can leave a leg ownerless by
# forgetting a flag. This script still passes the pid it finds
# to the watcher it spawns, because it is attached to the agent RIGHT NOW and
# the watcher is about to be backgrounded — if its parent exits first the
# watcher reparents to init and its own walk would find nothing.

# _owns_handle H — does the registry currently attribute H to OUR
# PROJECT_ROOT? A pgrep/marker match on H's watcher process is NOT proof of
# ownership by itself (Codex review finding 3): a derived or explicit handle
# can coincide with a DIFFERENT project's (a same-uid sibling watcher, or a
# stale row a truncation collision resurrected), which could otherwise report
# SURVIVED under someone else's identity or reclaim their row. This is an
# INDEPENDENT check against the shared registry, layered on top of
# comm-context.sh's own self-file-level root validation (which only proves
# the self-file is internally consistent, not that the registry still
# agrees).
_owns_handle() {
    [ -f "${REGISTRY:-}" ] || return 1
    local root
    root="$(jq -r --arg n "$1" '.agents[$n].root // ""' "$REGISTRY" 2>/dev/null)"
    [ -n "$root" ] && [ "$root" = "${PROJECT_ROOT:-}" ]
}

# _survived H — true only for a VALIDATED identity (never a merely-derived,
# speculative one — the caller only calls this with a pin or a validated
# self-file NAME) whose watcher is both alive AND ownership-checked.
# Recognizes both wake mechanisms (Codex review finding 11): Claude's
# comm-watch.sh and Codex's codex-watch.sh (argv shape `codex-watch.sh
# <handle> <pane>` — space-anchored on the right since a pane id always
# follows, the same anchoring purpose the `$`-anchor serves for comm-watch.sh
# alone). Windows liveness is PID-based (Codex review finding 4): comm-watch.sh
# writes its own pid to state/<handle>.watch once at startup; a killed
# watcher's pid fails `kill -0` immediately, unlike an age/heartbeat
# heuristic, which misreads in BOTH directions (a just-killed watcher still
# looks alive inside the window; a live one can look dead after a suspend or
# a slow poll cycle).
_survived() {
    local h="$1"
    [ -n "$h" ] || return 1
    _owns_handle "$h" || return 1
    local marker pid sid
    marker="$(_watch_marker "$h")"
    if [ -f "$marker" ]; then
        # Marker path, every platform (comm-watch.sh writes it everywhere):
        # line 1 pid (liveness), line 2 the session that armed it (identity,
        # 2026-09-10). A live watcher is OURS only if that session is this
        # one — an orphan from a previous session (its claude gone, the
        # watcher not: the killed capsule on a converged box, the pane-less
        # restart on a shared host) wakes nobody, so it must not count, and
        # it is reaped so it cannot fool the next check either. A marker with
        # no session line (a watcher older than this rule) keeps the old
        # liveness-only answer.
        # The read verifies the recorded pid IS a watcher for this handle,
        # not merely alive: on a shared home the marker outlives reboots, so a
        # reused pid would report SURVIVED for a session with no watcher at
        # all — deaf, and reporting healthy.
        #
        # And it is the NARROW read, a live `comm-wake`, because this answer
        # decides whether the row needs a PING watcher. The marker is shared —
        # comm-watch.sh and codex-watch.sh write it too — so the broad read
        # counted a surviving MONITOR as survival, the bootstrap reported
        # SURVIVED and armed nothing, and the row's only wake path was a
        # Monitor nobody re-arms: deaf within the half hour the harness gives
        # it. The narrow read returns 1 HERE, before the identity branch
        # below, so a live Monitor is neither counted nor killed and its
        # marker is untouched; the bootstrap falls through and arms a ping
        # watcher, whose own start claims the marker. Do not add a reap on
        # this path: a Monitor is a live wake path, not an orphan.
        pid="$(sot_wake_watcher_pid_for "$h")" || return 1
        sid="$(sed -n '2p' "$marker" 2>/dev/null)"
        if [ -n "${CLAUDE_CODE_SESSION_ID:-}" ] && [ -n "$sid" ] && [ "$sid" != "$CLAUDE_CODE_SESSION_ID" ]; then
            kill "$pid" 2>/dev/null || true
            echo "ORPHAN watcher pid=$pid (armed by a previous session) reaped — re-arming" >&2
            return 1
        fi
        return 0
    fi
    # NO MARKER, no watcher. The `pgrep` scan that used to answer this from
    # the outside is gone (messaging ruling §3-4): comm-wake.sh refuses to
    # start against a live marker itself, so the marker is the one answer, and
    # a process match was never one — it could not tell whose session armed it.
    return 1
}

# The work-state rule, printed on EVERY bootstrap outcome (fresh, survived,
# catch-up): the nav row colour is derived from it, and a session that
# launches a background job without stamping `waiting` shows green while the
# job runs — the exact miss the one-script rewrite's terse hint allowed.
_workstate_rule() {
    cat <<EOF
Work-state (nav row colour) — stamp it yourself with comm-status.sh <working|waiting|blocked|done|idle> "why":
  Priority: the white result badge above all, then red, then GREEN, then purple, then blue/gray.
  Red and green outrank purple STRUCTURALLY. While your turn is running the row is GREEN, even if a job you launched is also running — purple is a BETWEEN-turns colour, for when nothing of yours is running but something you launched is still outstanding. Red is a question that needs the user once your turn has ENDED; a question raised mid-turn shows green until the turn stops, which is correct: you are still working.
  Stamp waiting (purple) the moment you delegate; it is sticky across turns until you stamp working/idle/done when the job lands.
  A background job does NOT make you idle. If ANY item needs the user while jobs also run, the turn ends blocked (SITREP-QUESTION, the question first, jobs listed after); waiting only when nothing needs the user. Full mechanics: the sot-comm skill's references/work-state.md.
Turn end: when a turn CLOSES an effort (a result landed, a fix shipped, a diagnosis reached) or ends parked, its last block opens with a marker line — SITREP: <headline> (done) / SITREP-QUESTION: <question> (blocked) / SITREP-WAITING: <what for> (waiting) — followed by the sitrep chain in plain words (the sitrep skill: no hashes, paths, names, backticks or bullets). The Stop hook stamps the row from that line. A step in a live back-and-forth owes NO block: answer and end.
EOF
}

# Capabilities a session cannot discover on its own, printed on EVERY
# bootstrap outcome beside the work-state rule. Two sessions independently
# reported (2026-09-23) that they had never once used the workspace REPL --
# "invisible, not confusing" -- and both said the same thing unprompted:
# they read THIS output every time, and a skill they must already know to
# open would never have reached them. Quoted heredoc: nothing here expands.
_capability_lines() {
    cat <<'CAPEOF'
Julia: this workspace has a PERSISTENT REPL you can drive — sot-fe repl eval "$SOT_WORKSPACE_ID" --code '<code>' | sot-fe repl run <ws> <file.jl>. Packages stay loaded between runs and the call returns real output; use it instead of spawning julia for anything that re-pays a heavy package load. One eval at a time, and Main is shared with the owner's drawer — read the julia-repl skill before the first call.
CAPEOF
}

_context_block() {
    local h="$1" inbox
    # comm-lib.sh owns the platform branch (sot_fe_inbox_path): on Windows the
    # mail is the frontend's own file, everywhere else the per-handle one.
    inbox="$(sot_fe_inbox_path)"
    [ -n "$inbox" ] || inbox="${INBOX_DIR:-${SOT_COMM_HOME:-$HOME/.sot-comm}/inbox}/$h.jsonl"
    cat <<EOF
You are @$h. Inbox: $inbox
Verbs: comm-send.sh @<peer> "msg" | comm-poll.sh | comm-status.sh <working|waiting|blocked|done|idle> "why" | comm-list.sh
EOF
    _workstate_rule
    _capability_lines
    echo "Your inbox watcher never stopped: it survived this wipe. Do not re-join or re-poll."
}

if [ "$MODE" = "context" ]; then
    eval "$(SOT_COMM_READONLY=1 "$SCRIPT_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null || true
    H="${SOT_COMM_NAME:-${NAME:-}}"
    if [ -n "$H" ] && _survived "$H"; then
        echo "SURVIVED handle=$H"
        _context_block "$H"
    else
        echo "NOT SURVIVED handle=${H:-none} — run comm-session-start.sh (no flags) now to rebootstrap; a Monitor does not count as a wake path, and a wipe hook alone never re-joins/re-polls/re-arms."
    fi
    exit 0
fi

if [ "$MODE" = "catchup" ]; then
    eval "$("$SCRIPT_DIR/comm-context.sh")"
    H="${SOT_COMM_NAME:-${NAME:-}}"
    if [ -z "$H" ]; then
        echo "BOOTSTRAP handle=none poll=n/a identity=FAIL"
        exit 0
    fi

    # ONE reader, every platform. comm-poll.sh reads AND cursors both inboxes
    # on Windows (comm-lib.sh's sot_fe_* helpers), so the Windows branch that
    # used to live here was a second implementation of the same read -- and it
    # kept its own third cursor file, read/<handle>.fe-cursor, that no other
    # reader has ever looked at: catch-up marked frontend mail read where
    # comm-poll.sh and the turn-end hook could not see it.
    POLL_OUT="$("$SCRIPT_DIR/comm-poll.sh" 2>&1)"; poll_rc=$?
    if [ "$poll_rc" -ne 0 ]; then
        POLL_COUNT="ERR"
        printf '%s\n' "$POLL_OUT" >&2
    else
        POLL_COUNT="$(printf '%s\n' "$POLL_OUT" | grep -c '^\[' || true)"
        [ "${POLL_COUNT:-0}" -gt 0 ] 2>/dev/null && { echo "BACKLOG:"; printf '%s\n' "$POLL_OUT"; }
    fi

    echo "BOOTSTRAP handle=$H poll=${POLL_COUNT:-0} identity=ok"
    _workstate_rule
    _capability_lines
    exit 0
fi

# --- MODE=arm (phase 1, default) --------------------------------------------
# Diagnostics NOT suppressed: an ownership conflict (a differently-rooted
# self-file discarded here) must stay visible, because it changes what this
# script is allowed to do next (Codex review finding 2).
eval "$("$SCRIPT_DIR/comm-context.sh")"

PIN_NAME="${SOT_COMM_NAME:-}"

# A daemon-pinned capsule producer (SOT_COMM_SELF_FILE set, file absent) has
# NEVER joined — there is no watcher of its own to have survived.
COLD_PRODUCER=0
if [ -n "${SOT_COMM_SELF_FILE:-}" ] && [ ! -f "${SOT_COMM_SELF_FILE}" ]; then
    COLD_PRODUCER=1
fi

# Ownership conflict: something is recorded at THIS identity slot for a
# DIFFERENT, still-apparently-valid project (comm-context.sh discarded it —
# NAME came back empty despite the self-file existing), and no pin resolves
# the ambiguity. Mutating anything here — even a "harmless" bare join — would
# silently steal that slot. Refuse outright rather than guess (Codex review
# findings 1+2; this is exactly how an unpinned subagent clobbered a live
# coordinator session's identity during this PR's own testing).
if [ "$COLD_PRODUCER" = 0 ] && [ -z "$PIN_NAME" ] && [ -z "${NAME:-}" ] \
   && [ -n "${SELF_FILE:-}" ] && [ -f "$SELF_FILE" ]; then
    echo "BOOTSTRAP-ARM handle=none identity=FAIL MONITOR: n/a"
    echo "REFUSED: $SELF_FILE already names a different, validated identity (see the diagnostic line above) and no SOT_COMM_NAME/SOT_COMM_SELF_FILE pin was given — refusing to join over it. A subagent/lane launcher must pin a distinct SOT_COMM_NAME and, ideally, a private SOT_COMM_SELF_FILE of its own; see this skill's Identity line and references/reclaim-handle.md." >&2
    exit 0
fi

H=""
if [ -n "$PIN_NAME" ]; then
    H="$PIN_NAME"
elif [ -n "${NAME:-}" ]; then
    H="$NAME"
fi

# Survival short-circuits the bootstrap, so `_survived` answers the narrow
# question (a live comm-wake for this handle) in its one place; a surviving
# MONITOR falls through here and the bootstrap below arms a ping watcher.
if [ -n "$H" ] && _survived "$H"; then
    echo "SURVIVED handle=$H"
    # Manager review (S5): a survived session never re-runs comm-join.sh
    # (that's the whole point of "survived" — nothing was re-joined), so
    # this is the ONLY place a --continue restart re-declares to the
    # daemon. Idempotent (the daemon just overwrites the same value) and
    # best-effort: a failed declaration is recovered by the NEXT
    # comm-session-start the same way a fresh join's own agent.join retry
    # works — but (manager review, round 2) the failure itself must be
    # VISIBLE, checking the acknowledgment and warning exactly like
    # comm-join.sh's own fresh join does, not discarded silently.
    if join_ws="$(sot_capsule_workspace_id 2>/dev/null)" && [ -n "$join_ws" ]; then
        if ENDPOINT="$(sot_daemon_endpoint 2>/dev/null)" && [ -n "$ENDPOINT" ]; then
            join_frame="$(jq -nc --arg ws "$join_ws" --arg h "$H" \
                '{v:1, id:1, kind:"req", op:"agent.join", payload:{workspace_id:$ws, handle:$h}}')"
            join_res="$(sot_oneshot_request "$join_frame" "agent.join" || true)"
            if [ -z "$join_res" ] || ! printf '%s' "$join_res" | jq -e '.payload.ok == true' >/dev/null 2>&1; then
                echo "comm-session-start.sh: WARNING — could not declare '@$H' to the daemon (agent.join); it will retry at the next comm-session-start." >&2
            fi
        else
            echo "comm-session-start.sh: WARNING — this session's identity names row '$join_ws' but no daemon endpoint could be resolved; the daemon won't learn '@$H' until the next comm-session-start." >&2
        fi
    fi
    _context_block "$H"
    exit 0
fi

# --- deaf: cold start or --continue restart. Identity only. -----------------
# A session's handle is its row's handle everywhere, Windows included —
# comm-join.sh's own precedence (pin > validated self-file > derive) applies
# unchanged; nothing here derives a family handle for it.
JOIN_OUT="$("$SCRIPT_DIR/comm-join.sh" 2>&1)" || true
printf '%s\n' "$JOIN_OUT"
IDENTITY_MISMATCH=0
case "$JOIN_OUT" in
    *"still being heartbeated"*) IDENTITY_MISMATCH=1 ;;
esac
HANDLE="$(printf '%s\n' "$JOIN_OUT" | sed -n 's/^Joined sot-comm as @\([^ ]*\).*/\1/p' | head -n1)"
if [ -z "$HANDLE" ]; then
    # comm-join.sh failed outright (derivation exhausted every tier, or the
    # self-file write failed) — its own stderr (already printed above) names
    # the reason.
    echo "BOOTSTRAP-ARM handle=none identity=FAIL MONITOR: n/a"
    exit 0
fi

IDENTITY="ok"
[ "$IDENTITY_MISMATCH" = 1 ] && IDENTITY="MISMATCH"

# Claude sessions in a capsule row wake on a ping (comm-wake.sh --deliver
# ping) instead of paying a model turn every ~30 minutes to re-arm a
# harness Monitor (ADR 0047). Gated on $CLAUDE_CODE_SESSION_ID (the same
# signal _survived already uses to tell Claude from Codex): Codex's own
# skill starts its `--deliver full` watcher itself, right after this
# script returns — auto-starting a second one here for the same handle
# would race it. Outside a capsule row (no workspace id resolves) the
# MONITOR: line is unchanged, and Codex is untouched either way.
WAKE_ACTIVE=0
if [ -n "${CLAUDE_CODE_SESSION_ID:-}" ]; then
    CAPSULE_WS_ID=""
    if CAPSULE_WS_ID="$(sot_capsule_workspace_id 2>/dev/null)"; then
        # THE INVARIANT, stated once and here because this is where it is
        # decided: after this bootstrap, a capsule row running a Claude
        # session either HAS a live `comm-wake` ping watcher, or has printed
        # WAKE FAILED and the MONITOR command. There is no third state in
        # which the row claims a wake path it does not have.
        #
        # THREE DOORS can violate it and all three ask the same narrow
        # question — a live `comm-wake.sh` for this handle: this survived
        # claim, the guard inside comm-wake.sh (its marker branch and its
        # process scan), and the spawn's own did-it-come-up check below.
        #
        # SURVIVED is the door that is easiest to miss, and `_survived` is
        # where it is answered: narrowly, a live comm-wake for this handle. A
        # surviving MONITOR is not a surviving ping watcher — nothing re-arms
        # a Monitor after this release — so it falls through to the spawn
        # below, unreaped, and may keep running beside the ping watcher. A
        # doubled notice is the cheap failure; a deaf row is the expensive
        # one.
        if _survived "$HANDLE"; then
            WAKE_ACTIVE=1
        else
            # NO LIVE PROBE (2026-09-28). This used to resolve the endpoint and
            # require the daemon to answer `pty.screen` right now before it
            # would spawn a watcher at all, so a daemon silent for one second
            # cost the session its wake path for the rest of its life -- and
            # what it fell back to is the Monitor this whole mechanism exists
            # to replace. The watcher probes and RETRIES on its own now (it
            # backs off rather than exiting), so arming it is the honest claim
            # even against a daemon that has not answered yet; it resolves its
            # own endpoint and refuses on its own terms if it cannot.
            #
            # No owner, no watcher (messaging ruling §3): an untethered ping
            # watcher IS the immortal watcher -- it outlives the session, types
            # into a row that has moved on, and leaves a marker that makes the
            # next bootstrap report SURVIVED. comm-wake.sh refuses one anyway;
            # this keeps the fallback honest instead of spawning a doomed child.
            # It is also the LAST thing that can send a capsule row to the
            # Monitor, which is why it is the only test left here.
            WAKE_OWNER="$(sot_owner_pid || true)"
            if [ -n "$WAKE_OWNER" ]; then
                SOT_WORKSPACE_ID="$CAPSULE_WS_ID" nohup "$SCRIPT_DIR/comm-wake.sh" "$HANDLE" --deliver ping --owner "$WAKE_OWNER" \
                    </dev/null >/dev/null 2>&1 &
                # SPAWNED IS NOT ARMED. This used to claim WAKE the instant
                # nohup returned, so a watcher that died at startup -- a `set
                # -u` slip, a box with no jq, a bad path -- was announced as
                # "no Monitor needed" over a session with no wake path at all,
                # in the release whose whole point is removing the Monitor.
                # Looks healthy, is not.
                #
                # So: up to ONE SECOND, polled cheaply, exiting the moment a
                # live comm-wake.sh for this handle exists. Either process
                # passes -- the child just started, or the one already running
                # that made it refuse -- because the question is whether the
                # ROW has a wake path, not whose process provides it. The
                # predicate is the guard's own narrow one, so "a live ping
                # watcher for this handle" has a single definition.
                _wake_tries=0
                while [ "$_wake_tries" -lt 10 ]; do
                    if sot_live_wake_watcher_for "$HANDLE" "$$" any >/dev/null 2>&1; then
                        WAKE_ACTIVE=1
                        break
                    fi
                    sleep 0.1
                    _wake_tries=$((_wake_tries + 1))
                done
                if [ "$WAKE_ACTIVE" != 1 ]; then
                    # Loud, and on stdout beside the BOOTSTRAP-ARM line the
                    # session actually reads: a noisy fallback beats a quiet
                    # lie. The Monitor line below is printed as usual.
                    echo "WAKE FAILED: comm-wake.sh did not come up for @$HANDLE within 1s — arming the Monitor instead"
                fi
            else
                echo "wake: no owning claude/codex ancestor found; falling back to the Monitor" >&2
            fi
        fi
    fi
fi

if [ "$WAKE_ACTIVE" = 1 ]; then
    echo "BOOTSTRAP-ARM handle=$HANDLE identity=$IDENTITY WAKE: comm-wake.sh (ping; no Monitor needed)"
else
    # printf %q quotes BOTH the executable path and the handle (Codex
    # review finding 8): an unquoted command breaks under a spaced
    # installation path. comm-watch.sh itself honors $SOT_COMM_HOME for
    # the inbox/marker it reads — nothing extra to thread through here.
    MONITOR_CMD="$(printf '%q %q' "$SCRIPT_DIR/comm-watch.sh" "$HANDLE")"
    echo "BOOTSTRAP-ARM handle=$HANDLE identity=$IDENTITY MONITOR: $MONITOR_CMD (persistent; if the harness ends it, re-arm on the notice - this hook warns if you miss one)"
fi
_workstate_rule
_capability_lines
# A good join (this point is only ever reached after printing BOOTSTRAP-ARM
# above) is success, full stop -- never let a well-behaved but non-integer-0
# exit status trailing off the end of the script (a heredoc's `cat`, some
# future addition here) silently turn a good bootstrap into a caller-visible
# failure.
exit 0
