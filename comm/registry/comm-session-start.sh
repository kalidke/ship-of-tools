#!/usr/bin/env bash
# comm-session-start.sh — the deterministic sot-comm receive-bootstrap.
#
#   comm-session-start.sh             resolve identity, join,
#                                      print ONE BOOTSTRAP-ARM line and STOP.
#                                      The daemon wakes the row; there is
#                                      nothing to arm.
#
# IDENTITY PRECEDENCE (Codex review findings 1–3): pin ($SOT_COMM_NAME, or a
# private $SOT_COMM_SELF_FILE whose file already exists and validates) →
# validated self-file NAME → fresh derivation. This script NEVER passes an
# explicit `--name` to comm-join.sh — comm-join.sh's OWN precedence (--name
# arg > $SOT_COMM_NAME env > self-file NAME > derive) already implements the
# same order correctly; manufacturing an explicit --name here from a
# lower-priority source (the bug this PR shipped with) can override a real
# launcher pin. A session whose slot another project holds pins a distinct
# $SOT_COMM_NAME and, outside a row, its own private $SOT_COMM_SELF_FILE;
# nothing started inside a session joins (it uses no mail) — see
# references/reclaim-handle.md. When neither is
# given and the self-file already names a DIFFERENT, validated identity, this
# script REFUSES to join at all rather than silently stealing that slot (this
# is exactly how PR1's own coordinator session was clobbered by an unpinned
# subagent during testing — see the implementation report).
#
# See comm/adapters/claude/sot-session-start/SKILL.md for what the calling
# skill does with each printed line.
set -uo pipefail
# Any other argument is a mode this script does not have: fail before anything
# runs or writes, never fall through to the joining default.
case "${1:-}" in
    "") ;;
    *) echo "usage: comm-session-start.sh" >&2; exit 2 ;;
esac
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=comm-lib.sh
source "$SCRIPT_DIR/comm-lib.sh"
# A missing jq, flock or perl means this session never sees mail: say so loudly
# on stdout, where the calling skill reads, and stop.
_start_tools="$(sot_mail_tools)"
# shellcheck disable=SC2086
if ! _start_missing="$(sot_require_tools "start the comm session" $_start_tools 2>&1)"; then
    printf '%s\n' "$_start_missing"
    exit 1
fi

# The work-state rule, printed on EVERY bootstrap outcome:
# the nav row colour is derived from it, and a session that launches a
# background job without stamping `waiting` shows green while the job runs — the exact miss the one-script
# rewrite's terse hint allowed.
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

# --- bootstrap -------------------------------------------------------------
# Diagnostics NOT suppressed: an ownership conflict (a differently-rooted
# self-file discarded here) must stay visible, because it changes what this
# script is allowed to do next (Codex review finding 2).
eval "$("$SCRIPT_DIR/comm-context.sh")"

PIN_NAME="${SOT_COMM_NAME:-}"

# A daemon-pinned capsule producer (SOT_COMM_SELF_FILE set, file absent) has
# NEVER joined — there is no identity of its own to have kept.
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
    echo "BOOTSTRAP-ARM handle=none identity=FAIL WAKE: n/a"
    echo "REFUSED: $SELF_FILE already names a different, validated identity (see the diagnostic line above) and no SOT_COMM_NAME pin was given — refusing to join over it. The session's launcher pins a distinct SOT_COMM_NAME and, outside a row, a private SOT_COMM_SELF_FILE of its own; nothing started inside a session joins. See this skill's Identity line and references/reclaim-handle.md." >&2
    exit 0
fi

# --- identity only: a cold start or a --continue restart. ------------------
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
    echo "BOOTSTRAP-ARM handle=none identity=FAIL WAKE: n/a"
    exit 0
fi

IDENTITY="ok"
[ "$IDENTITY_MISMATCH" = 1 ] && IDENTITY="MISMATCH"

echo "BOOTSTRAP-ARM handle=$HANDLE identity=$IDENTITY WAKE: daemon"
sot_handoff_line "$HANDLE"
_workstate_rule
_capability_lines
# A good join (this point is only ever reached after printing BOOTSTRAP-ARM
# above) is success, full stop -- never let a well-behaved but non-integer-0
# exit status trailing off the end of the script (a heredoc's `cat`, some
# future addition here) silently turn a good bootstrap into a caller-visible
# failure.
exit 0
