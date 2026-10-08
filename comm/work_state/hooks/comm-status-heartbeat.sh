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
# Source of truth: comm/work_state/hooks/comm-status-heartbeat.sh in
# Ship of Tools, deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail
# A headless claude launched BY comm tooling (the turn auditor's tier-2 call)
# runs these same hooks under the parent's identity: its prompt hook painted
# the parent's row working and its Stop hook floored it to done, one second
# after the parent's own marker had stamped blocked (field report, 2026-09-18).
# The launcher sets SOT_COMM_HOOKS=off; every status hook stands down on it.
[ "${SOT_COMM_HOOKS:-}" = off ] && exit 0
# Everything this hook writes is the comm folder's, so private (ADR 0049, User isolation); it starts only comm scripts.
umask 077
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

[ -f "$REGISTRY" ] || exit 0

# Read stdin (the hook's JSON envelope) up front, before the early-throttle
# exit below can short-circuit: the AskUserQuestion answer check right after
# needs it every call, throttle or no throttle. Cached in a variable, not
# re-read: stdin is a pipe, and jq would see EOF on a second read.
_envelope="$(cat)"
tool="$(printf '%s' "$_envelope" | jq -r '.tool_name // ""' 2>/dev/null || true)"

# comm-lib.sh, in the comm home's bin first (update_comm puts every script
# there), then next to this file: the fallback pair the hook uses for
# comm-context.sh too. It is only ever sourced in a subshell.
hb_lib="$COMM_HOME/bin/comm-lib.sh"
[ -r "$hb_lib" ] || hb_lib="$SELF_DIR/comm-lib.sh"
# A second agent inside the session (codex exec, claude -p) changes nothing under
# the comm home: not the owner's AskUserQuestion marker, not the state directory,
# not the row's tick. So this gate comes before every write below; it needs only
# $$. Status 1 is a child, silent; any other nonzero (an ancestry that cannot be
# read, a lib too old to hold the gate) says why on stderr, and the hook still
# stands down.
_agent_gate() {
    local _why _rc
    _why="$( ( . "$hb_lib" >/dev/null 2>&1 || exit 127; sot_require_agent ) 2>/dev/null )"; _rc=$?
    case "$_rc" in
        0) ;;
        1) exit 0 ;;
        *) [ -n "$_why" ] || _why="could not check whether this process is its own session's agent; comm-lib.sh did not load or lacks the check"
           echo "sot-comm: $_why" >&2; exit 0 ;;
    esac
}

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
#
# Identity isn't the only thing that can be wrong, though: a completed
# AskUserQuestion elsewhere — a foreign dialog this row never opened, or one
# whose row is unrelated — must never be able to clear a DIFFERENT, still-
# open question just because both tool calls share this branch (a badge
# showed idle with a real question waiting because exactly this happened —
# field report, 2026-09-27). Proof is the marker
# comm-status-blocked.sh's PreToolUse drops, keyed by the SAME `tool_use_id`
# this completion carries: only consuming that marker earns the `prompt`.
# No marker (no PreToolUse ever opened this exact dialog) means no answer.
if [ "$tool" = AskUserQuestion ]; then
    _agent_gate
    _tool_use_id="$(printf '%s' "$_envelope" | jq -r '.tool_use_id // ""' 2>/dev/null || true)"
    _askq_marker="$COMM_HOME/state/askq-$(printf '%s' "$_tool_use_id" | tr -c 'A-Za-z0-9._-' '_').marker"
    if [ -n "$_tool_use_id" ] && [ -f "$_askq_marker" ]; then
        rm -f "${_askq_marker:?}" 2>/dev/null || true
        COMM_STATUS_ORIGIN=user "$COMM_HOME/bin/comm-status.sh" prompt >/dev/null 2>&1 || true
    fi
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
_agent_gate
mkdir -p "$COMM_HOME/state" 2>/dev/null || true
touch -- "$_hb_tick" 2>/dev/null || true
NAME=""
# Context resolution runs under comm-lib's one command-bound owner, sot_bounded.
# Its default command budget is 10 seconds, followed by bounded TERM/KILL
# confirmation. SOT_HB_CTX_TIMEOUT_TICKS is a test seam: positive counts round
# up from 50 ms ticks to whole seconds; zero skips resolution.
# Keep stdout and stderr in private regular files so an unfinished descendant
# cannot hold the hook's own output capture open. Replay a finite stderr
# snapshot after the bound returns; failed bounds discard all context stdout.
_hb_context_budget() {
    _ctx_ticks="${SOT_HB_CTX_TIMEOUT_TICKS:-200}"
    case "$_ctx_ticks" in ''|*[!0-9]*) _ctx_ticks=200 ;; esac
    while [ "${_ctx_ticks#0}" != "$_ctx_ticks" ]; do _ctx_ticks="${_ctx_ticks#0}"; done
    [ -n "$_ctx_ticks" ] || return 1
    # Bash's signed integer range on the supported 64-bit platforms. Check the
    # decimal text before arithmetic; quotient/remainder avoids ticks + 19.
    if [ "${#_ctx_ticks}" -gt 19 ] || { [ "${#_ctx_ticks}" -eq 19 ] && [[ "$_ctx_ticks" > 9223372036854775807 ]]; }; then
        echo "sot-comm: heartbeat context budget is unusable; heartbeat skipped" >&2
        return 1
    fi
    _ctx_ticks=$((10#$_ctx_ticks))
    _ctx_secs=$((_ctx_ticks / 20 + (_ctx_ticks % 20 != 0)))
}
_hb_context() {
    # Capture these before the library assigns SELF_DIR to its identity cache.
    local _ctx_exec="$SELF_DIR/comm-context.sh" _ctx_ticks _ctx_secs
    local _ctx_state _ctx_out _ctx_err _ctx_rc _ctx_bytes _ctx=""
    [ -x "$_ctx_exec" ] || return 0
    _hb_context_budget || return 0
    _ctx_state="$(cd "$COMM_HOME/state" 2>/dev/null && pwd -P)" || {
        echo "sot-comm: heartbeat context artifact could not be created or removed; heartbeat skipped" >&2
        return 0
    }
    _ctx_out="$_ctx_state/.hb-ctx-$$"
    _ctx_err="$_ctx_state/.hb-ctx-$$.err"
    if ! ( set -C; : >"$_ctx_out" ) 2>/dev/null; then
        echo "sot-comm: heartbeat context artifact could not be created or removed; heartbeat skipped" >&2
        return 0
    fi
    if ! ( set -C; : >"$_ctx_err" ) 2>/dev/null; then
        rm -f "${_ctx_out:?}" 2>/dev/null || true
        echo "sot-comm: heartbeat context artifact could not be created or removed; heartbeat skipped" >&2
        return 0
    fi
    (
        . "$hb_lib" || exit 125
        type sot_bounded >/dev/null 2>&1 || exit 125
        sot_bounded "$_ctx_secs" "$_ctx_exec"
    ) >"$_ctx_out" 2>"$_ctx_err"
    _ctx_rc=$?
    _ctx_bytes="$(wc -c <"$_ctx_err")"
    head -c "$_ctx_bytes" "$_ctx_err" >&2
    case "$_ctx_rc" in
        124|125|127|137|129|130|143)
            echo "sot-comm: heartbeat context unavailable (command bound status $_ctx_rc); heartbeat skipped" >&2 ;;
        *) _ctx="$(cat "$_ctx_out" 2>/dev/null)" ;;
    esac
    if ! rm -f "${_ctx_out:?}" "${_ctx_err:?}" 2>/dev/null; then
        echo "sot-comm: heartbeat context artifact could not be created or removed; heartbeat skipped" >&2
        _ctx=""
    fi
    [ -n "$_ctx" ] && eval "$_ctx" 2>/dev/null || true
}
_hb_context
[ -n "${NAME:-}" ] || exit 0

# The row and the write below are comm-lib.sh's sot_registry_read and
# registry_replace, sourced in a subshell (hb_lib, above).
# sot_registry_read: 0 my row on stdout, 1 no row, 2 unreadable (a lib that
# cannot be sourced is 2 too, never "no row"); only a row prints anything.
row="$( ( . "$hb_lib" >/dev/null 2>&1 || exit 2; sot_registry_read "$NAME" ) | jq -r '(.floor // "") + "|" + (.status_at // "")' 2>/dev/null || true)"
[ -n "$row" ] || exit 0

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
# Best-effort merge under the registry lock (comm-lib.sh's with_lock, a file
# naming its holder). One try: a held lock is skipped — the next tool call
# retries within a minute anyway — but a dead holder on this machine is still
# reclaimed, because the reclaim runs on the first failed take. hb_merge is
# comm-lib.sh's registry_replace, the one registry write: on any failure
# nothing is written. Silent: the subshell's output is dropped, and no readable
# comm-lib.sh means skip.
hb_merge() {
    registry_replace \
        'if .agents[$n] and .agents[$n].floor
         then .agents[$n] += {status_at:$t, last_seen:$t} else . end' \
        --arg n "$NAME" --arg t "$ts"
}
[ -r "$hb_lib" ] || exit 0
( . "$hb_lib" >/dev/null 2>&1 && SOT_LOCK_WAIT_SECS=0 with_lock hb_merge ) >/dev/null 2>&1
exit 0
