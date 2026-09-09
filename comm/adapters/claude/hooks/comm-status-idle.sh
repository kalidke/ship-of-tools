#!/usr/bin/env bash
# comm-status-idle.sh — Claude Code `Stop` hook for comm agents. Three jobs:
#
#   (0) CLOSING MARKER (2026-09-09). A turn whose last reply opens a line with
#       `SITREP:` / `SITREP-QUESTION:` / `SITREP-WAITING:` has DECLARED its
#       end state (done / blocked / waiting) and written the report that state
#       demands (sitrep skill). The hook stamps that state EXPLICITLY, with the
#       rest of the marker line as the nav-row summary, so the chat and the
#       row come from one line and cannot disagree. A marker ends the hook: no
#       floor, no nudge, no auditor. Conversely, a HUMAN turn that ends with an
#       explicit blocked / waiting / done row and NO marker gets one nudge
#       naming the shape it owes (a machine wake never does — a relay ack on a
#       parked row is not a report). See sot-comm references/work-state.md.
#
#   (1) NUDGE (reinforce self-report). If a JOINED comm agent ends a turn whose
#       last reply contains a `?` and it did NOT already self-mark blocked/waiting,
#       remind it — via a Stop `decision:block` whose reason is fed back to the
#       model — to run `comm-status.sh blocked "<q>"` IF that `?` was a real
#       question for the user. It is a REMINDER, never an auto-mark: the MODEL
#       decides whether the `?` was actually a blocking question (the hook cannot
#       tell rhetorical from real), so there is no false-positive block — at worst a one-line
#       "that was rhetorical" continuation. Plain-text questions fire no automatic
#       signal (only the AskUserQuestion tool does), so without this the row looks
#       idle while the agent is actually waiting.
#
#   (2) TURN-END FLOOR. Otherwise floor the row with a SOFT `done`: blue
#       ("finished a turn you asked for, unread") when the row was working from
#       a genuine prompt, gray otherwise -- comm-status.sh's soft-floor guard
#       decides, and never clobbers a deliberate blocked/waiting/done (owner
#       decision 2026-09-08, BLUE/GRAY = UNREAD/READ; see that script's header).
#
# Wired as a global Stop hook in ~/.claude/settings.json (comm.jl / update_comm).
# It fires at every turn-end in EVERY session. CRITICAL SAFETY: the nudge (which
# BLOCKS the stop / forces a continuation) fires ONLY for a joined comm agent — a
# non-comm session (human shell, etc.) takes the plain idle-floor path and is
# NEVER blocked. Every failure path also falls through to the floor + exit 0, so
# the hook can never wedge a turn.
#
# Source of truth: comm/adapters/claude/hooks/comm-status-idle.sh in Ship of Tools,
# deployed to ~/.sot-comm/bin by ShipTools.update_comm(). Edit it there.
set -uo pipefail
HOME_DIR="${SOT_COMM_HOME:-$HOME/.sot-comm}"
STATUS="$HOME_DIR/bin/comm-status.sh"
REGISTRY="$HOME_DIR/registry.json"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Deployment-order tolerance (Codex review, #223): an OLDER comm-status.sh
# guards only a soft `idle`, so sending it `done` would paint a blocked or
# waiting row blue. Send `done` only to a script that has the soft floor.
FLOOR=idle; grep -q soft_floor "$STATUS" 2>/dev/null && FLOOR=done
turn_floor() { [ -x "$STATUS" ] && COMM_STATUS_SOFT=1 "$STATUS" "$FLOOR" >/dev/null 2>&1 || true; }

# Stop-hook input (JSON on stdin): {stop_hook_active, transcript_path, ...}.
input="$(cat 2>/dev/null || true)"
jqget() { printf '%s' "$input" | jq -r "$1" 2>/dev/null || true; }

# Comm-agent gate (the safety line): resolve our handle; ONLY a joined comm agent
# with a registry row is eligible for the nudge. Anyone else → plain idle floor,
# NEVER a block.
NAME=""
# comm-context lives in the comm home's bin (where update_comm deploys every
# script together); next to this file is the deployed layout too, kept as the
# fallback. In the repo checkout the hooks dir holds only hooks.
CTX="$HOME_DIR/bin/comm-context.sh"; [ -x "$CTX" ] || CTX="$SELF_DIR/comm-context.sh"
[ -x "$CTX" ] && eval "$("$CTX" 2>/dev/null)" 2>/dev/null || true
if [ -z "${NAME:-}" ] || ! jq -e --arg n "${NAME:-}" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1; then
    turn_floor; exit 0
fi

tp="$(jqget '.transcript_path // empty')"

# The WHOLE text of the last assistant message (the closing reply). The
# legacy code took only its last line, which is where the marker never is.
last_text=""
if [ -n "$tp" ] && [ -r "$tp" ]; then
    last_text="$(tail -n 400 "$tp" 2>/dev/null \
        | jq -c 'select(.type=="assistant") | [.message.content[]? | select(.type=="text") | .text] | join("\n")' 2>/dev/null \
        | tail -n 1 | jq -r '.' 2>/dev/null)"
fi

# (0) CLOSING MARKER: first line opening with SITREP[-QUESTION|-WAITING]:
# (optionally bold-wrapped). State from the marker, summary from the rest of
# the line — or the next non-empty line when the marker stands alone.
marker_state=""; marker_summary=""
if [ -n "$last_text" ]; then
    marker_state="$(printf '%s\n' "$last_text" | awk '
        /^[[:space:]]*(\*\*)?SITREP(-QUESTION|-WAITING)?:/ {
            m=$0; sub(/^[[:space:]]*(\*\*)?SITREP/, "", m)
            if (m ~ /^-QUESTION:/) print "blocked"; else if (m ~ /^-WAITING:/) print "waiting"; else print "done"
            exit }')"
    if [ -n "$marker_state" ]; then
        marker_summary="$(printf '%s\n' "$last_text" | awk '
            found { if ($0 ~ /[^[:space:]]/) { print; exit } ; next }
            /^[[:space:]]*(\*\*)?SITREP(-QUESTION|-WAITING)?:/ {
                sub(/^[[:space:]]*(\*\*)?SITREP(-QUESTION|-WAITING)?:[[:space:]]*/, "")
                sub(/[[:space:]]*(\*\*)?[[:space:]]*$/, "")
                if ($0 ~ /[^[:space:]]/) { print; exit } ; found=1 }')"
    fi
fi
if [ -n "$marker_state" ]; then
    # Explicit (not soft): the marker IS the model's report. `waiting` sets
    # the sticky purple; `blocked` keeps a marker underneath as today.
    [ -x "$STATUS" ] && "$STATUS" "$marker_state" "$marker_summary" >/dev/null 2>&1 || true
    exit 0
fi

# Loop guard: if we are ALREADY in a stop-hook continuation, never re-nudge —
# floor + let the turn end (one nudge per turn, no infinite continue-loop).
[ "$(jqget '.stop_hook_active // false')" = "true" ] && { turn_floor; exit 0; }

# A parked end state without its report. The row is blocked / waiting / done
# at Stop time only when the MODEL stamped it (or a sticky waiting held
# through the turn); on a HUMAN turn that owes the matching closing block.
# One nudge, then the continuation's marker stamps the row above. A machine
# wake (relay, Monitor, notification) never nudges: floor and end.
cur="$(jq -r --arg n "$NAME" '.agents[$n] | (.state // "") + "|" + (.turn_origin // "")' "$REGISTRY" 2>/dev/null || echo "|")"
origin="${cur#*|}"; cur="${cur%%|*}"
case "$cur" in
    blocked|waiting|done)
        if [ "$origin" = user ]; then
            case "$cur" in
                blocked) owed='SITREP-QUESTION: <the exact question, one sentence>  then the context needed to answer it cold: what was being done, the options and what follows from each, the default if unanswered, what is irreversible' ;;
                waiting) owed='SITREP-WAITING: <what is being waited on, one sentence>  then EVERY armed monitor, background job, subagent and peer request: what it is, what completion looks like, expected duration, the fallback if it never lands, and what happens when it does' ;;
                *)       owed='SITREP: <one-line headline>  then the sitrep chain (the sitrep skill): issue in context, diagnosis, design, result with its scale, interpretation, plan' ;;
            esac
            jq -nc --arg s "$cur" --arg o "$owed" '{
              decision: "block",
              reason: ("Your row ends this turn as `" + $s + "` but the reply carries no closing marker. Write the closing block now, as the last thing in your reply, opening with the marker line:  " + $o + ".  The Stop hook stamps the row from that line (the rest of the marker line is the nav summary). If the state is wrong, run comm-status.sh with the right one and still close with the matching marker.")
            }'
            exit 0
        fi
        turn_floor; exit 0 ;;
esac

# TIERED TURN AUDITOR (v1, 2026-07-02): deterministic pre-filters + a
# conservative Haiku judge (comm-turn-auditor.sh) check the turn for misses —
# real turn-ending question w/o blocked, user-facing artifact never surfaced
# to the FE (show-result), background task armed w/o waiting. Contract:
#   rc 0 + output → confirmed findings, nudge with them;
#   rc 0 + empty  → audited CLEAN (Haiku judged rhetorical-vs-real etc.) —
#                   skip the legacy grep nudge, plain floor;
#   rc 3          → auditor off/unavailable → legacy '?' grep nudge below.
AUDITOR="$SELF_DIR/comm-turn-auditor.sh"
if [ -x "$AUDITOR" ] && [ -n "$tp" ]; then
    findings="$("$AUDITOR" "$NAME" "$tp" 2>/dev/null)"; arc=$?
    if [ "$arc" -eq 0 ]; then
        if [ -n "$findings" ]; then
            # MSYS2 argv-conversion guard: on Windows git-bash, a NATIVE
            # jq.exe rewrites any argv element starting with "/" into a
            # Windows path before jq sees it -- findings is free text from
            # an LLM judge and could legitimately start with "/", so it
            # must not reach jq via --arg. This hook stays standalone (no
            # comm-lib.sh source, matching every other hook here), so the
            # fix is inlined rather than calling comm-lib.sh's
            # sot_jq_rawfile.
            _findings_file="$(mktemp "${TMPDIR:-/tmp}/sot-comm-idle-findings.XXXXXX" 2>/dev/null)"
            if [ -n "$_findings_file" ] && printf '%s' "$findings" > "$_findings_file" 2>/dev/null; then
                jq -nc --rawfile f "$_findings_file" '{
                  decision: "block",
                  reason: ("Turn-end audit: " + $f + " -- IF a finding is real, act on it now AND clearly RESTATE it for the user: blocked -> restate the exact question you are awaiting (one standalone sentence, as BOTH the comm-status summary and the final line of your reply); waiting -> state plainly what is being monitored and what completion looks like (same two places); artifact -> badge it via the show-result skill. IF a finding is wrong (rhetorical question, artifact already shown, job already done), just end the turn normally. This audit will not re-fire for the same situation.")
                }'
                rm -f "$_findings_file"
            else
                # Temp file failed -- degrade rather than risk a corrupted
                # --arg on Windows; the legacy grep fallback below still runs.
                rm -f "$_findings_file" 2>/dev/null
                turn_floor
            fi
            exit 0
        fi
        turn_floor; exit 0
    fi
fi

# LEGACY FALLBACK (auditor disabled/unavailable): grep the last reply for `?`.
if printf '%s' "$last_text" | grep -q '?'; then
    # NUDGE — block the stop with a reminder. The model gates: self-report or not.
    jq -nc '{
      decision: "block",
      reason: "Reminder: your last reply contains a question mark, and a plain-text question (not the AskUserQuestion tool) fires no automatic frontend signal. IF you are ending this turn AWAITING THE USER on a blocking question, run  ~/.sot-comm/bin/comm-status.sh blocked \"<the question>\"  now so your row shows red on the frontend. IF the question(s) were rhetorical or already answered, just end the turn normally — this nudge will not fire again this turn."
    }'
    exit 0
fi

# No question → plain idle floor.
turn_floor
exit 0
