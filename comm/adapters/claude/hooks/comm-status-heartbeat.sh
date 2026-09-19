#!/usr/bin/env bash
# comm-status-heartbeat.sh — Claude Code `PostToolUse` hook. Two jobs:
#
#   (1) The AskUserQuestion ANSWER (ADR 0044 amendment): that tool's
#       PostToolUse is the owner typing the answer — the session yielding to
#       the harness pause is over. Sends `prompt` with origin `user` for that
#       tool call, exactly as if a fresh UserPromptSubmit had fired, and
#       exits (no heartbeat write this call).
#   (2) The HEARTBEAT: writes NO fact. It only refreshes `status_at` on tool
#       activity, THROTTLED to once per 60s, so a legitimately-busy session on
#       a long turn (heavy Julia runs) doesn't wilt white — the nav wilts a
#       `working` row whose status_at is older than 10 min
#       (AGENT_STALE_MINUTES), and status_at was only written at turn START
#       (the maintainer, 2026-07-03: "why does a peer session keep reverting
#       to white while it's working"). It never sets a floor — a subagent or
#       lane sharing the lead's handle would otherwise paint a stopped, red
#       or purple lead green for hours; a hook-less machine wake therefore
#       runs without green, and the Stop hook's origin correction + `stop`
#       still close it correctly.
#
# Cheap by construction: the no-op path (not a comm agent / no floor / stamp
# fresh) is a couple of jq reads; the registry write happens at most once a
# minute. Always exits 0 — a hook must never wedge a turn.
#
# Source of truth: comm/adapters/claude/hooks/comm-status-heartbeat.sh in
# Ship of Tools, deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail
# A headless claude launched BY comm tooling (the turn auditor's tier-2 call)
# runs these same hooks under the parent's identity: its prompt hook painted
# the parent's row working and its Stop hook floored it to done, one second
# after the parent's own marker had stamped blocked (field report, 2026-09-18).
# The launcher sets SOT_COMM_HOOKS=off; every status hook stands down on it.
[ "${SOT_COMM_HOOKS:-}" = off ] && exit 0
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

[ -f "$REGISTRY" ] || exit 0

# Read stdin (the hook's JSON envelope) up front, before the early-throttle
# exit below can short-circuit: the AskUserQuestion answer check right after
# needs it every call, throttle or no throttle.
tool="$(jq -r '.tool_name // ""' 2>/dev/null || true)"

# The AskUserQuestion ANSWER (ADR 0044 amendment): this tool's PostToolUse
# fires once the owner has typed the answer and the harness resumes — the
# session is no longer yielding. Sends `prompt` (origin user) exactly like a
# fresh turn start, once per dialog, and skips the heartbeat write below
# entirely (comm-status.sh takes the spinning lock; a tool-call-rate write
# here would be the wrong cost model for it). Runs BEFORE the early throttle
# and this hook's own identity resolution below: a teammate's or subagent's
# tool call sharing this session id can re-touch the throttle tick (below)
# while the dialog is open, and gating the answer on that tick or on NAME
# here would let such a call swallow the owner's answer (review finding 1,
# 2026-09-19). comm-status.sh resolves identity and self-gates on its own
# registry row, so no NAME is needed here.
if [ "$tool" = AskUserQuestion ]; then
    COMM_STATUS_ORIGIN=user "$COMM_HOME/bin/comm-status.sh" prompt >/dev/null 2>&1 || true
    exit 0
fi

# EARLY THROTTLE (2026-09-17): everything below -- comm-context.sh above all --
# costs ~7s on Windows (git rev-parse + hostname + jq + sourcing comm-lib.sh,
# each a process spawn Git Bash charges dearly for). This hook fires on EVERY
# PostToolUse, so that cost was landing on every single tool call and shells
# piled up faster than they retired. The registry refresh below is already
# throttled to 60s -- but that check runs AFTER the expensive part, so it saved
# a write, never the work. Gate the whole body on a cheap mtime stamp instead.
#
# 10s, not the 60s used below: the state-change path (promote waiting->working)
# deliberately bypasses that throttle so the nav colour flips on the FIRST tool
# call of a turn, and a 60s gate here would defeat that. 10s keeps the flip
# effectively immediate while dropping ~85% of invocations. Anti-wilt is
# unaffected either way (it fires at 10 MINUTES of zero activity).
_hb_key="${CLAUDE_CODE_SESSION_ID:-${SOT_WORKSPACE_ID:-$PPID}}"
_hb_tick="$COMM_HOME/state/hb-$(printf '%s' "$_hb_key" | tr -c 'A-Za-z0-9._-' '_').tick"
if [ -f "$_hb_tick" ] && [ -n "$(find "$_hb_tick" -newermt '-10 seconds' 2>/dev/null)" ]; then
    exit 0
fi
mkdir -p "$COMM_HOME/state" 2>/dev/null || true
touch -- "$_hb_tick" 2>/dev/null || true
NAME=""
# TIMEOUT GUARD (2026-09-17): comm-context.sh was observed hung on Windows,
# and because this hook fires on EVERY PostToolUse a stalled child piles up
# one wedged bash per tool call -- ~150 of them on one box, which starved Git
# Bash startup past 120s and stalled the harness's own Bash tool. "A hook must
# never wedge a turn" (header, above), so cap the child and carry on
# contextless if it stalls: a missing NAME just exits 0 a few lines down,
# which is the same no-op as not being a comm agent.
# Bash-native watchdog, not an external `timeout`: in Git Bash a bare
# `timeout` resolves to C:\WINDOWS\system32\timeout.exe, which is NOT
# coreutils and takes no command, and /usr/bin/timeout isn't guaranteed
# either. Run comm-context.sh in the background, poll for up to 10s, kill it
# if it's still alive past that -- collect its output only when it finished
# on its own (a non-zero exit from a fast, legitimate no-context run still
# has its output used, matching the old unconditional `|| true`).
if [ -x "$SELF_DIR/comm-context.sh" ]; then
    _ctx_out="$COMM_HOME/state/.hb-ctx-$$"
    "$SELF_DIR/comm-context.sh" >"$_ctx_out" 2>/dev/null &
    _ctx_pid=$!
    _waited=0
    while kill -0 "$_ctx_pid" 2>/dev/null && [ "$_waited" -lt 10 ]; do
        sleep 1
        _waited=$((_waited + 1))
    done
    if kill -0 "$_ctx_pid" 2>/dev/null; then
        kill "$_ctx_pid" 2>/dev/null
        _ctx=""
    else
        _ctx="$(cat "$_ctx_out" 2>/dev/null)"
    fi
    wait "$_ctx_pid" 2>/dev/null
    rm -f "$_ctx_out" 2>/dev/null
    [ -n "${_ctx:-}" ] && eval "$_ctx" 2>/dev/null || true
fi
[ -n "${NAME:-}" ] || exit 0

row="$(jq -r --arg n "$NAME" '.agents[$n] | if . then (.floor // "") + "|" + (.status_at // "") else "" end' "$REGISTRY" 2>/dev/null || true)"
[ -n "$row" ] || exit 0

# DEAF-SESSION WARNING (2026-09-15): a session whose harness inbox Monitor
# died still looks alive here — this hook keeps running, the registry row
# keeps updating below — while comm-listen.sh keeps filing relay traffic
# into its inbox with nothing left to wake it on. The bug is silence, not a
# crash, so this must run BEFORE the state/staleness early exits below
# (the case statement and the throttle's `exit 0`), because those two exit
# on exactly the busy-but-silent rows this warning exists to catch.
#
# Liveness check duplicated from comm-session-start.sh's `_survived()` (its
# twin — comm/core/scripts/comm-session-start.sh, the marker read around
# lines 97-122) because hooks are standalone by design and don't source
# comm-lib.sh, so both copies are kept in sync by hand (same convention as
# the machine-origin pattern lists duplicated across comm-status-idle.sh).
# UNLIKE that twin, this check is PID-liveness ONLY — no session-id
# comparison: a subagent/lane inherits its parent's handle but gets its own
# $CLAUDE_CODE_SESSION_ID, and no env signal proves subagent-ness
# ($CLAUDE_CODE_CHILD_SESSION is set in a parent session's own hook shell
# too), so a lane must read the parent's still-live watcher as proof this
# handle isn't deaf, even when the marker names a different session.
if [ -n "${CLAUDE_CODE_SESSION_ID:-}" ]; then
    watch_marker="$COMM_HOME/state/$NAME.watch"
    watcher_alive=0
    if [ -f "$watch_marker" ]; then
        wpid="$(sed -n '1p' "$watch_marker" 2>/dev/null)"
        [[ "$wpid" =~ ^[0-9]+$ ]] && kill -0 "$wpid" 2>/dev/null && watcher_alive=1
    fi
    if [ "$watcher_alive" = 0 ]; then
        # Own throttle stamp (NOT the registry's status_at) so a busy row
        # that legitimately skips the registry write below (lock contention,
        # a `done`/`blocked` state) still only warns once per 10 minutes.
        warn_stamp="$COMM_HOME/state/$NAME.watchwarn"
        warn_age=999999
        if [ -f "$warn_stamp" ]; then
            wmtime="$(stat -c '%Y' "$warn_stamp" 2>/dev/null || echo 0)"
            warn_age=$(( $(date -u +%s) - wmtime ))
        fi
        if [ "$warn_age" -ge 600 ]; then
            echo "comm-status-heartbeat: no live inbox watcher for @$NAME — you are deaf; run $COMM_HOME/bin/comm-session-start.sh (starts the ping wake on a capsule row, else prints the Monitor command)" >&2
            mkdir -p "$(dirname "$warn_stamp")" 2>/dev/null
            touch -- "$warn_stamp" 2>/dev/null || true
        fi
    fi
fi

floor="${row%%|*}"; at="${row#*|}"

# No floor → no turn is running → nothing to refresh (a subagent/lane
# sharing the lead's handle must never paint a stopped, red or purple lead
# green here — the heartbeat sets no fact, ever). A fresh stamp → nothing to
# do; refresh only once the stamp is 60s or older (avoids registry churn on
# every tool call of a busy turn).
[ -n "$floor" ] || exit 0
now_s=$(date -u +%s)
at_s=$(date -u -d "$at" +%s 2>/dev/null || echo 0)
[ $((now_s - at_s)) -ge 60 ] || exit 0

ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
# Best-effort merge under the registry's mkdir-spinlock convention
# (comm-lib.sh's with_lock uses LOCKDIR="$COMM_HOME/.registry.lock" — a
# DIRECTORY). No spinning here: if the lock is held, just skip — the next
# tool call retries within a minute anyway.
LOCKDIR="$COMM_HOME/.registry.lock"
if mkdir "$LOCKDIR" 2>/dev/null; then
    trap 'rmdir "$LOCKDIR" 2>/dev/null' EXIT
    jq --arg n "$NAME" --arg t "$ts" \
       'if .agents[$n] and .agents[$n].floor
        then .agents[$n] += {status_at:$t, last_seen:$t} else . end' \
       "$REGISTRY" > "$REGISTRY.hb.tmp" 2>/dev/null && mv "$REGISTRY.hb.tmp" "$REGISTRY"
    rmdir "$LOCKDIR" 2>/dev/null
    trap - EXIT
fi
exit 0
