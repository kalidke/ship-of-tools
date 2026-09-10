#!/usr/bin/env bash
# comm-session-start.sh — the deterministic sot-comm receive-bootstrap, split
# into TWO phases (Codex review finding 5) so a message can never land before
# the Monitor that would wake on it exists, and so the wake-proof selftest
# always has a live Monitor to actually prove:
#
#   comm-session-start.sh             phase 1 ("arm"): resolve identity, start
#                                      the listener, print ONE line and STOP —
#                                      either SURVIVED (nothing to do) or
#                                      BOOTSTRAP-ARM (arm the printed MONITOR
#                                      command, THEN run phase 2).
#   comm-session-start.sh --catch-up  phase 2, run only after the Monitor is
#                                      armed: poll the backlog, selftest (now
#                                      safe — the Monitor exists to catch its
#                                      wake frame), and the sot layer. Prints
#                                      the final verdict.
#   comm-session-start.sh --context   read-only: print a short context block
#                                      if survived, else say so and name the
#                                      phase-1 re-run. NEVER joins, listens,
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

IS_WINDOWS=0
_sot_is_windows && IS_WINDOWS=1

# Windows FE-family identity is by ROLE, never merely "is this Windows"
# (Codex review finding 7) — comm-session-skill.sh is the single source of
# truth for "is this session the frontend driver," already used to route
# hooks/launchers; reused here instead of re-deriving the same judgment.
SKILL_NAME="$("$SCRIPT_DIR/comm-session-skill.sh" 2>/dev/null || true)"
IS_SOT=0
IS_FE_ROLE=0
case "$SKILL_NAME" in
    /sot-fe-session-start) IS_SOT=1; IS_FE_ROLE=1 ;;
    /sot-be-session-start) IS_SOT=1 ;;
esac

_watch_marker() { printf '%s/state/%s.watch\n' "${SOT_COMM_HOME:-$HOME/.sot-comm}" "$1"; }

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
    if [ "$IS_WINDOWS" = 1 ]; then
        local marker pid
        marker="$(_watch_marker "$h")"
        [ -f "$marker" ] || return 1
        pid="$(cat "$marker" 2>/dev/null)"
        [[ "$pid" =~ ^[0-9]+$ ]] || return 1
        kill -0 "$pid" 2>/dev/null
    else
        local h_re
        h_re="$(printf '%s' "$h" | sed 's/\./\\./g')"
        pgrep -u "$(id -un)" -f "comm-watch\\.sh ${h_re}\$" >/dev/null 2>&1 \
            || pgrep -u "$(id -un)" -f "codex-watch\\.sh ${h_re} " >/dev/null 2>&1
    fi
}

# The work-state rule, printed on EVERY bootstrap outcome (fresh, survived,
# catch-up): the nav row colour is derived from it, and a session that
# launches a background job without stamping `waiting` shows green while the
# job runs — the exact miss the one-script rewrite's terse hint allowed.
_workstate_rule() {
    cat <<EOF
Work-state (nav row colour) — stamp it yourself with comm-status.sh <working|waiting|blocked|done|idle> "why":
  blocked (red: needs the user) > waiting (purple: a job/subagent/peer YOU launched is still running — stamp it the moment you delegate; sticky until you stamp working/idle/done when the job lands) > working (green) > idle.
  A background job does NOT make you idle. Full mechanics: the sot-comm skill's references/work-state.md.
Turn end: when a turn CLOSES an effort (a result landed, a fix shipped, a diagnosis reached) or ends parked, its last block opens with a marker line — SITREP: <headline> (done) / SITREP-QUESTION: <question> (blocked) / SITREP-WAITING: <what for> (waiting) — followed by the sitrep chain in plain words (the sitrep skill: no hashes, paths, names, backticks or bullets). The Stop hook stamps the row from that line. A step in a live back-and-forth owes NO block: answer and end.
EOF
}

_context_block() {
    local h="$1" inbox
    if [ "$IS_WINDOWS" = 1 ]; then
        inbox="${LOCALAPPDATA:-${XDG_STATE_HOME:-$HOME/.local/state}}/sot/fe-inbox.jsonl"
    else
        inbox="${INBOX_DIR:-${SOT_COMM_HOME:-$HOME/.sot-comm}/inbox}/$h.jsonl"
    fi
    cat <<EOF
You are @$h. Inbox: $inbox
Verbs: comm-relay.sh send @<peer> "msg" | comm-poll.sh | comm-status.sh <working|waiting|blocked|done|idle> "why" | comm-list.sh | bus.sh sync
EOF
    _workstate_rule
    cat <<EOF
Your Monitor (comm-watch.sh $h) never stopped: it survived this wipe. Do not re-join, re-listen, or re-poll.
EOF
}

if [ "$MODE" = "context" ]; then
    eval "$(SOT_COMM_READONLY=1 "$SCRIPT_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null || true
    H="${SOT_COMM_NAME:-${NAME:-}}"
    if [ -n "$H" ] && _survived "$H"; then
        echo "SURVIVED handle=$H"
        _context_block "$H"
    else
        echo "NOT SURVIVED handle=${H:-none} — run comm-session-start.sh (no flags) now to rebootstrap; a wipe hook alone never re-joins/re-polls/re-arms."
    fi
    exit 0
fi

if [ "$MODE" = "catchup" ]; then
    eval "$("$SCRIPT_DIR/comm-context.sh")"
    H="${SOT_COMM_NAME:-${NAME:-}}"
    if [ -z "$H" ]; then
        echo "BOOTSTRAP handle=none poll=n/a selftest=down bus=n/a identity=FAIL"
        exit 0
    fi

    # Selftest runs AFTER the Monitor is armed (phase 1 already printed
    # BOOTSTRAP-ARM and the skill armed it before running this phase — Codex
    # review finding 5): its own wake-proof frame now has a live watcher to
    # catch it, instead of racing a Monitor that doesn't exist yet.
    SELFTEST_OUT="$("$SCRIPT_DIR/comm-listen.sh" --selftest 2>&1)"; rc=$?
    printf '%s\n' "$SELFTEST_OUT"
    case "$rc" in
        0) SELFTEST="ok" ;;
        3) SELFTEST="retry" ;;
        *) SELFTEST="down" ;;
    esac

    if [ "$IS_WINDOWS" = 1 ]; then
        # Windows catch-up reads/cursors fe-inbox.jsonl directly — comm-poll.sh
        # reads the Linux per-handle inbox, the wrong file here entirely
        # (Codex review finding 7), missing every message received while
        # this session was down. Family-label admission mirrors
        # comm-watch.sh's own rule: only a win-fe-family handle also wakes on
        # the bare `win-fe` broadcast label; any other handle sees only
        # `to:<itself>`. Cursor is an append-position (a line count), not a
        # timestamp, so it can't skip or duplicate across a race.
        FE_INBOX="${LOCALAPPDATA:-${XDG_STATE_HOME:-$HOME/.local/state}}/sot/fe-inbox.jsonl"
        CURSOR_DIR="${SOT_COMM_HOME:-$HOME/.sot-comm}/read"
        mkdir -p "$CURSOR_DIR" 2>/dev/null || true
        CURSOR_FILE="$CURSOR_DIR/$H.fe-cursor"
        TOTAL="$(wc -l < "$FE_INBOX" 2>/dev/null || echo 0)"
        LAST="$(cat "$CURSOR_FILE" 2>/dev/null || echo 0)"
        [[ "$LAST" =~ ^[0-9]+$ ]] || LAST=0
        [ "$LAST" -gt "$TOTAL" ] && LAST=0
        POLL_COUNT=0
        if [ "$TOTAL" -gt "$LAST" ]; then
            case "$H" in
                win-fe*) fam_filter='(.to // "") == $me or (.to // "") == "win-fe"' ;;
                *)       fam_filter='(.to // "") == $me' ;;
            esac
            NEW="$(sed -n "$((LAST + 1)),\$p" "$FE_INBOX" 2>/dev/null | while IFS= read -r l; do
                printf '%s' "$l" | jq -rc --arg me "$H" \
                    "select(.from != \$me and ($fam_filter)) | \"[\\(.ts // \"?\")] [\\(.from)] \\(.text)\"" 2>/dev/null
            done)"
            POLL_COUNT="$(printf '%s\n' "$NEW" | grep -c '^\[' || true)"
            [ "${POLL_COUNT:-0}" -gt 0 ] 2>/dev/null && { echo "BACKLOG:"; printf '%s\n' "$NEW"; }
        fi
        printf '%s' "$TOTAL" > "$CURSOR_FILE" 2>/dev/null || true
    else
        POLL_OUT="$("$SCRIPT_DIR/comm-poll.sh" 2>&1)"; poll_rc=$?
        if [ "$poll_rc" -ne 0 ]; then
            POLL_COUNT="ERR"
            printf '%s\n' "$POLL_OUT" >&2
        else
            POLL_COUNT="$(printf '%s\n' "$POLL_OUT" | grep -c '^\[' || true)"
            [ "${POLL_COUNT:-0}" -gt 0 ] 2>/dev/null && { echo "BACKLOG:"; printf '%s\n' "$POLL_OUT"; }
        fi
    fi

    BUS="n/a"
    if [ "$IS_SOT" = 1 ]; then
        "$SCRIPT_DIR/comm-relay.sh" send @win-fe "[ack?] $H receive path armed" >/dev/null 2>&1 || true
        # Peek only: `bus.sh sync --count` NEVER advances the bus cursor
        # (Codex review finding 9 — the old version did, permanently hiding
        # entries this verdict line never actually showed anyone). A nonzero
        # count here is a durable prompt to run `bus.sh sync` (or
        # /bus-sync) for real, never a silent acknowledgement.
        BUS="$("$SCRIPT_DIR/bus.sh" sync --count 2>/dev/null || echo "n/a")"
    fi

    echo "BOOTSTRAP handle=$H poll=${POLL_COUNT:-0} selftest=$SELFTEST bus=$BUS identity=ok"
    _workstate_rule
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
    echo "BOOTSTRAP-ARM handle=none listener=n/a identity=FAIL MONITOR: n/a"
    echo "REFUSED: $SELF_FILE already names a different, validated identity (see the diagnostic line above) and no SOT_COMM_NAME/SOT_COMM_SELF_FILE pin was given — refusing to join over it. A subagent/lane launcher must pin a distinct SOT_COMM_NAME and, ideally, a private SOT_COMM_SELF_FILE of its own; see this skill's Identity line and references/reclaim-handle.md." >&2
    exit 0
fi

H=""
if [ -n "$PIN_NAME" ]; then
    H="$PIN_NAME"
elif [ -n "${NAME:-}" ]; then
    H="$NAME"
fi

if [ -n "$H" ] && _survived "$H"; then
    echo "SURVIVED handle=$H"
    _context_block "$H"
    exit 0
fi

# --- deaf: cold start or --continue restart. Identity + listener only. -----
# A genuinely fresh FE-role session (nothing pinned, no validated self-file
# yet) derives the win-fe-<host> family handle — mirrors the Rust frontend's
# own self_comm_handle() exactly. Set as an ENV pin, never an explicit
# --name (Codex review finding 1): comm-join.sh's bare-join precedence
# (--name arg > $SOT_COMM_NAME env > self-file NAME > derive) already slots
# this correctly BELOW a validated self-file and ABOVE plain basename
# derivation — an explicit --name would instead rank ABOVE self-file, which
# is backwards.
if [ -z "$PIN_NAME" ] && [ -z "${NAME:-}" ] && [ "$IS_FE_ROLE" = 1 ]; then
    export SOT_COMM_NAME="win-fe-$( (hostname -s 2>/dev/null || hostname) | tr '[:upper:]' '[:lower:]' )"
fi

JOIN_OUT="$("$SCRIPT_DIR/comm-join.sh" 2>&1)" || true
printf '%s\n' "$JOIN_OUT"
IDENTITY_MISMATCH=0
case "$JOIN_OUT" in
    *"ALREADY RUNNING"*) IDENTITY_MISMATCH=1 ;;
esac
HANDLE="$(printf '%s\n' "$JOIN_OUT" | sed -n 's/^Joined sot-comm as @\([^ ]*\).*/\1/p' | head -n1)"
if [ -z "$HANDLE" ]; then
    # comm-join.sh failed outright (derivation exhausted every tier, or the
    # self-file write failed) — its own stderr (already printed above) names
    # the reason.
    echo "BOOTSTRAP-ARM handle=none listener=n/a identity=FAIL MONITOR: n/a"
    exit 0
fi

LISTEN_OUT="$("$SCRIPT_DIR/comm-listen.sh" 2>&1)"; listen_rc=$?
printf '%s\n' "$LISTEN_OUT"
if [ "$IS_WINDOWS" = 1 ]; then
    LISTENER_STATE="n/a"
elif [ "$listen_rc" -eq 0 ]; then
    LISTENER_STATE="up"
else
    LISTENER_STATE="down"
fi

IDENTITY="ok"
[ "$IDENTITY_MISMATCH" = 1 ] && IDENTITY="MISMATCH"

# printf %q quotes BOTH the executable path and the handle (Codex review
# finding 8): an unquoted command breaks under a spaced installation path.
# comm-watch.sh itself honors $SOT_COMM_HOME for the inbox/marker it reads —
# nothing extra to thread through here.
MONITOR_CMD="$(printf '%q %q' "$SCRIPT_DIR/comm-watch.sh" "$HANDLE")"
echo "BOOTSTRAP-ARM handle=$HANDLE listener=$LISTENER_STATE identity=$IDENTITY MONITOR: $MONITOR_CMD"
_workstate_rule
