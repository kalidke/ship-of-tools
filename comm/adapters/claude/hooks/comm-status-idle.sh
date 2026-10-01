#!/usr/bin/env bash
# comm-status-idle.sh — Claude Code `Stop` hook for comm agents. Four jobs,
# numbered by topic. They RUN in this order: the inbox read and (2)'s mail
# block, then a lock fault's once-block, on EVERY turn, a marker turn
# included; then (0); then (1) and the other nudges; then (3). While an inbox
# lock fault lasts, every block carries its warning first.
#
#   (0) CLOSING MARKER (2026-09-09). A turn whose last reply opens a line with
#       `SITREP:` / `SITREP-QUESTION:` / `SITREP-WAITING:` has DECLARED its
#       end state (done / blocked / waiting) and written the report that state
#       demands (sitrep skill). The hook stamps that state EXPLICITLY, with the
#       rest of the marker line as the nav-row summary, so the chat and the
#       row come from one line and cannot disagree. Once the inbox check has
#       let the turn end, a marker ends the hook: no nudge, no auditor.
#       Unread mail, or a lock fault's once-block, holds a marker turn first,
#       unstamped; the turn end that finally passes stamps from the LAST marker
#       anywhere in the turn, because this hook's own held-turn notices do not
#       start a new turn (the slice pass below). Conversely, a HUMAN turn
#       that ends with an explicit blocked / waiting / done row and NO marker
#       gets one nudge naming the shape it owes (a machine wake never does — a
#       relay ack on a parked row is not a report). See sot-comm references/work-state.md.
#       Two refinements (2026-09-10, owner: "sessions are not strictly
#       following the sitrep rules" + "don't need that formal thing during a
#       back and forth"): (a) a HUMAN turn that was an EFFORT — many tool
#       calls or a long wall time — and ends green with no marker gets ONE
#       soft nudge: close with the sitrep block if the turn closed an effort,
#       end normally if it was a step in a live exchange. Before this, only a
#       row the session had already parked was ever nudged, so the sessions
#       that never stamp were never reminded. Short turns are never nudged.
#       (b) WITHDRAWN the same day: a plain-language lint that sent a block
#       carrying identifiers back to be rewritten. A Stop send-back can only
#       APPEND a continuation, so the rewrite landed as a second, different
#       block under the first — the owner saw two waiting reports in a row.
#       Any nudge on a turn that already has its block is a duplicate by
#       construction; the language rules live in the sitrep skill only.
#
#       ARTIFACT AUDIT EXCEPTION (2026-09-14, owner: "the hook should eval if
#       something should be displayed in the preview" -- a session announced a
#       design brief in a file but never showed it). A marker still stamps the
#       row and ends the flow, EXCEPT: before exiting it runs the artifact-only
#       tier of comm-turn-auditor.sh once, and blocks with a show-result
#       reminder if that finds an unsurfaced result -- the row stays stamped
#       from the marker, only the missed badge gets a nudge.
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
#   (2) NEW MAIL. A turn does not end while directed sot-comm mail sits unread:
#       the hook reads this handle's inbox and, when a line newer than that
#       file's read cursor is addressed to it, blocks with "run comm-poll.sh". That is how a BUSY
#       session is reached — no watcher, no keystrokes, no human (the messaging
#       ruling, 2026-09-26). See the branches below for the exact rules.
#
#   (3) TURN END. Otherwise send `stop`: comm-status.sh sets `done` only when
#       `floor` was `user` and neither `question` nor `waiting` is set, then
#       clears `floor` — the row is a set of facts, and the reduction (not
#       this hook) decides blue/gray/red/purple from whatever facts remain
#       (ADR 0044 amendment 2026-09-19).
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
# A headless claude launched BY comm tooling (the turn auditor's tier-2 call)
# runs these same hooks under the parent's identity: its prompt hook painted
# the parent's row working and its Stop hook floored it to done, one second
# after the parent's own marker had stamped blocked (field report, 2026-09-18).
# The launcher sets SOT_COMM_HOOKS=off; every status hook stands down on it.
[ "${SOT_COMM_HOOKS:-}" = off ] && exit 0
HOME_DIR="${SOT_COMM_HOME:-$HOME/.sot-comm}"
STATUS="$HOME_DIR/bin/comm-status.sh"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# comm-lib.sh, deployed layout first, then next to this file. It is only ever
# sourced in a subshell: for the tool list, the registry reads (sot_registry_read,
# or sot_registry_bytes without jq; both retry a failed or empty read) and the
# mail gate below.
FE_LIB="$HOME_DIR/bin/comm-lib.sh"; [ -r "$FE_LIB" ] || FE_LIB="$SELF_DIR/comm-lib.sh"

# A second agent inside the session (codex exec, claude -p) is not the row's
# agent: no block, floor or stamp. The gate comes before the context call and
# needs only $$. Status 1 is a child, silent; any other nonzero (an ancestry
# that cannot be read, a lib too old to hold the gate) says why in a
# systemMessage, and the hook still stands down.
_why="$( ( . "$FE_LIB" >/dev/null 2>&1 || exit 127; sot_require_agent ) 2>/dev/null )"; _rc=$?
case "$_rc" in
    0) ;;
    1) exit 0 ;;
    *) [ -n "$_why" ] || _why="could not check whether this process is its own session's agent; comm-lib.sh did not load or lacks the check"
       _why="$(printf '%s' "$_why" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"
       printf '{"systemMessage":"sot-comm: %s"}\n' "$_why"
       exit 0 ;;
esac

# Every Stop ends with `stop`, whatever else this hook did first (the marker
# stamp, the nudge continuation): it sets `done` only when `floor` was `user`
# and neither `question` nor `waiting` is set, then clears `floor` — a fact
# already set (by the marker, or an earlier declaration) survives untouched.
turn_floor() { [ -x "$STATUS" ] && "$STATUS" stop >/dev/null 2>&1 || true; }
# Every block this hook prints goes through print_block, which also records
# its exact reason, one JSON string per line, in $fb_file (named below): the
# transcript brings the block back as a "Stop hook feedback:" record, and the
# slice pass knows it by that text as this hook's own, not a new turn. A Stop
# that prints no block removes the file (the EXIT trap below). A failed write
# fails open: that feedback then reads as a prompt, as it did before.
blocked="" fb_file=""
print_block() {  # BLOCK_JSON
    blocked=1
    [ -n "$fb_file" ] || { printf '%s\n' "$1"; return 0; }
    { mkdir -p "$HOME_DIR/state" && printf '%s' "$1" | jq -c '.reason' >> "$fb_file"; } 2>/dev/null || true
    printf '%s\n' "$1"
}
# Every other block (all but the lock fault's own once-block, whose reason is
# the warning) goes through emit_block: while an inbox lock fault lasts, its
# warning ($lock_warn, read below) prefixes the reason. With no fault the JSON
# passes through byte for byte. The warning never begins with `/`, so --arg is
# safe from MSYS2's argv conversion.
# A missing tool ($tool_warn, set below) prefixes the reason the same way.
emit_block() {  # BLOCK_JSON
    local w="${tool_warn:+$tool_warn }${lock_warn:-}"
    w="${w% }"
    if [ -z "$w" ]; then print_block "$1"
    else print_block "$(printf '%s' "$1" | jq -c --arg w "$w" '.reason = $w + " " + .reason')"; fi
}

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
# The tools this hook runs. A session whose jq, flock or perl is missing never
# sees its mail, so the hook SAYS so instead of passing: it must not need the
# missing tool to do it, so the joined-agent test below falls back to grep and
# the block is printed by hand. flock and perl are the inbox lock, Linux only.
# The list and the message are comm-lib.sh's (sot_mail_tools, sot_require_tools),
# read in a subshell like the other lib reads here.
tool_miss=""; tool_warn=""
_mail_tools="$( . "$FE_LIB" 2>/dev/null; sot_mail_tools 2>/dev/null)" || _mail_tools=""
[ -n "$_mail_tools" ] || _mail_tools="jq"
for _t in $_mail_tools; do
    command -v "$_t" >/dev/null 2>&1 && continue
    tool_miss="${tool_miss:+$tool_miss }$_t"
done
if [ -n "$tool_miss" ]; then
    # shellcheck disable=SC2086
    tool_warn="$( . "$FE_LIB" 2>/dev/null
        sot_require_tools "check mail at turn end" $tool_miss 2>&1 | tr '\n' ' ')"
    tool_warn="${tool_warn% }"
fi
# sot_registry_read: 0 a row, 1 no row, 2 unreadable (a lib that cannot be
# sourced is 2 too, never "no row"). Only "no row" ends here; unreadable goes
# on to the mail gate, which reads the inbox files, not the registry. Without
# jq (which sot_registry_read needs), grep over sot_registry_bytes: 1 no match, 2
# unreadable. The bytes are captured first: piped straight into grep, pipefail
# would let grep's 1 turn "unreadable" into "no row".
_reg_rc=0
if [ -z "${NAME:-}" ]; then
    :
elif command -v jq >/dev/null 2>&1; then
    ( . "$FE_LIB" >/dev/null 2>&1 || exit 2; sot_registry_read "$NAME" >/dev/null ) || _reg_rc=$?
else
    ( . "$FE_LIB" >/dev/null 2>&1 || exit 2; b="$(sot_registry_bytes)" || exit 2
      printf '%s' "$b" | grep -q "\"${NAME}\"[[:space:]]*:" ) || _reg_rc=$?
fi
if [ -z "${NAME:-}" ] || [ "$_reg_rc" -eq 1 ]; then
    turn_floor; exit 0
fi

# Clear the heartbeat's own throttle stamp for this session key (same key
# formula as comm-status-heartbeat.sh's _hb_key) so a tick left over from
# the tail of THIS turn can't gate the early-throttle exit on the NEXT
# turn's first tool call, which would otherwise skip the promote-to-working
# flip and leave the row purple/idle through real work.
rm -f "${HOME_DIR:?}/state/hb-$(printf '%s' "${CLAUDE_CODE_SESSION_ID:-${SOT_WORKSPACE_ID:-$PPID}}" | tr -c 'A-Za-z0-9._-' '_').tick" 2>/dev/null

tp="$(jqget '.transcript_path // empty')"
# The session's key for the tick files and the feedback record, which must be
# STABLE for the session: $PPID is not (every hook run is its own process), so
# the transcript path — one file per session — is the fallback before it.
mail_key="${CLAUDE_CODE_SESSION_ID:-${SOT_WORKSPACE_ID:-}}"
[ -n "$mail_key" ] && [ "$mail_key" != "nopane" ] || mail_key="${tp##*/}"
[ -n "$mail_key" ] || mail_key="$PPID"
# With no transcript path the key can fall to $PPID, which no later run shares,
# so nothing could find the record again: record nothing, and remove nothing.
# A missing tool: ONE block per episode, through a tick file keyed like the
# lock fault's (handle and session) and holding the missing tools, so a new
# tool going missing blocks again; every later block is prefixed by the warning
# (emit_block) and the first turn end with the tools present clears the tick.
# Failing open, like the other ticks, when the tick cannot be recorded. The
# block is printed by hand: jq itself may be the missing tool.
if [ -n "$tp" ]; then
    fb_file="$HOME_DIR/state/stop-feedback-$(printf '%s' "$mail_key" | tr -c 'A-Za-z0-9._-' '_').jsonl"
    trap '[ -n "$blocked" ] || rm -f -- "${fb_file:?}" 2>/dev/null' EXIT
fi

tool_tick="$HOME_DIR/state/tool-fault-$(printf '%s' "$NAME.$mail_key" | tr -c 'A-Za-z0-9._-' '_').tick"
if [ -z "$tool_miss" ]; then
    rm -f "${tool_tick:?}" 2>/dev/null
else
    echo "$tool_warn" >&2
    # Never inside a stop-hook continuation: with jq missing there may be no
    # stable session key, so the tick could never match and every Stop would
    # block again.
    case "$input" in *'"stop_hook_active":true'*) turn_floor; exit 0 ;; esac
    if [ "$(cat "$tool_tick" 2>/dev/null || true)" != "$tool_miss" ]; then
        mkdir -p "$HOME_DIR/state" 2>/dev/null || true
        if printf '%s' "$tool_miss" 2>/dev/null > "$tool_tick"; then
            print_block "$(printf '{"decision":"block","reason":"%s"}' "$tool_warn")"
            exit 0
        fi
    fi
fi

# The current turn's slice: everything after the last HUMAN/machine prompt (a
# `user` record whose content is a string / carries no tool_result -- tool
# results are `user` records too). One jq pass over that slice feeds two
# things: the effort counter (tool calls + wall seconds) and the closing text
# the marker check below searches. A prompt not found in the tail means the
# turn is longer than the tail: an effort by construction (tools/secs pinned
# to a 9999 sentinel, as before) -- but the text is still joined from
# whatever the tail holds, since a stale prefix beats a missed marker.
#
# The text is EVERY assistant text block in the slice, joined in order, not
# just the last assistant record's (2026-09-14, live incident: a long reply
# opened with SITREP-WAITING: but the hook nudged "no closing marker" anyway).
# A long reply can stream across several assistant transcript records for one
# logical turn, and a marker opening an earlier record was silently dropped
# when only the last record was read.
#
# The turn is the LOGICAL turn: a block this hook printed comes back as a
# `user` record whose content is "Stop hook feedback:\n" plus the block's
# reason, and a record whose content EQUALS that for a reason in $fb_file is
# not a prompt, so the slice starts after the last prompt that is not one of
# them. The marker a held turn closed with therefore still counts at the turn
# end that passes. Any other "Stop hook feedback:" record, one this hook did
# not record, is a prompt (the origin correction below takes it as machine).
# The recorded reasons reach jq on stdin, ahead of the tail and each wrapped
# as {stop_feedback: R}, never through argv: a reason can hold free text, and
# MSYS2 rewrites an argument that looks like a path.
EFFORT_TOOLS=8; EFFORT_SECS=300
turn_tools=0; turn_secs=0; last_text=""; prompt_text=""
if [ -n "$tp" ] && [ -r "$tp" ]; then
    turn_json="$( { jq -cR 'fromjson? | strings | {stop_feedback: .}' "$fb_file" 2>/dev/null
                    tail -n 3000 "$tp" 2>/dev/null; } | jq -sc '
        def is_prompt: .type=="user" and ((.message.content|type)=="string"
            or (([.message.content[]? | .type] | index("tool_result")) == null));
        def is_own_feedback($fb): (.message.content) as $c | .isMeta == true and ($c|type)=="string"
            and any($fb[]; "Stop hook feedback:\n" + . == $c);
        def secs: sub("\\.[0-9]+Z$"; "Z") | (try fromdateiso8601 catch 0);
        def prompt_of: (.message.content) as $c
            | if ($c|type)=="string" then $c
              else ([$c[]? | select(.type=="text") | .text] | join("\n")) end;
        [.[] | .stop_feedback? // empty] as $fb
        | [.[] | select(.stop_feedback? == null)]
        | ([to_entries[] | select(.value | is_prompt and (is_own_feedback($fb) | not)) | .key] | last) as $h
        | (if $h == null then . else .[$h+1:] end) as $turn
        | ([$turn[] | select(.type=="assistant") | .message.content[]? | select(.type=="text") | .text] | join("\n")) as $text
        | if $h == null then {tools: 9999, secs: 9999, text: $text, prompt: ""}
          else
            ([$turn[] | select(.type=="assistant") | .message.content[]? | select(.type=="tool_use")] | length) as $n
            | ((.[$h].timestamp // "") | if . == "" then 0 else secs end) as $t0
            | (([$turn[] | select(.type=="assistant") | .timestamp // empty] | last // "") | if . == "" then 0 else secs end) as $t1
            | {tools: $n, secs: (if $t0 > 0 and $t1 > $t0 then $t1 - $t0 else 0 end), text: $text, prompt: (.[$h] | prompt_of)}
          end' 2>/dev/null)"
    turn_tools="$(printf '%s' "$turn_json" | jq -r '.tools // 0' 2>/dev/null)"
    turn_secs="$(printf '%s' "$turn_json" | jq -r '.secs // 0' 2>/dev/null)"
    last_text="$(printf '%s' "$turn_json" | jq -r '.text // ""' 2>/dev/null)"
    # Last prompt record's own text (2026-09-15, turn-origin correction below):
    # empty when the turn is longer than the tail ($h was null) -- the origin
    # override is skipped in that case, same as before this change.
    prompt_text="$(printf '%s' "$turn_json" | jq -r '.prompt // ""' 2>/dev/null)"
fi
# The Stop payload's own `last_assistant_message` is appended (2026-09-14): the
# hook can fire before the final reply record reaches the transcript, and a
# marker in that unflushed reply was nudged as missing. The transcript slice
# still covers a reply split across records; the payload covers the race.
lam="$(jqget '.last_assistant_message // empty')"
if [ -n "$lam" ]; then
    last_text="${last_text}
${lam}"
    case "$turn_tools" in ''|*[!0-9]*) turn_tools=0 ;; esac
    case "$turn_secs" in ''|*[!0-9]*) turn_secs=0 ;; esac
fi

# (0) CLOSING MARKER: the LAST line in the turn opening with
# SITREP[-QUESTION|-WAITING]: (optionally bold-wrapped). State from that
# marker, summary from the rest of its line — or the next non-empty line when
# the marker stands alone.
marker_state=""; marker_summary=""
if [ -n "$last_text" ]; then
    marker_state="$(printf '%s\n' "$last_text" | awk '
        /^[[:space:]]*(#+[[:space:]]*)?(\*\*)?SITREP(-QUESTION|-WAITING)?(\*\*)?:/ {
            m=$0; sub(/^[[:space:]]*(#+[[:space:]]*)?(\*\*)?SITREP/, "", m); gsub(/\*\*/, "", m)
            if (m ~ /^-QUESTION:/) s = "blocked"; else if (m ~ /^-WAITING:/) s = "waiting"; else s = "done" }
        END { if (s != "") print s }')"
    if [ -n "$marker_state" ]; then
        marker_summary="$(printf '%s\n' "$last_text" | awk '
            /^[[:space:]]*(#+[[:space:]]*)?(\*\*)?SITREP(-QUESTION|-WAITING)?(\*\*)?:/ {
                sub(/^[[:space:]]*(#+[[:space:]]*)?(\*\*)?SITREP(-QUESTION|-WAITING)?(\*\*)?:[[:space:]]*/, "")
                sub(/[[:space:]]*(\*\*)?[[:space:]]*$/, "")
                s = $0; found = ($0 !~ /[^[:space:]]/); next }
            found && /[^[:space:]]/ { s = $0; found = 0 }
            END { print s }')"
    fi
fi
# (2) NEW MAIL — delivery to a BUSY session, at the turn boundary (messaging
# ruling §2, 2026-09-26). The inbox is a file this session can read, so nothing
# has to reach into it: a turn does not END while directed mail sits unread. The
# block's reason is fed back to the model, which polls, acts, then ends the
# turn — and polling is what advances the cursor, so this terminates by
# construction. The read runs on EVERY turn, BEFORE the closing marker and
# every nudge below (mail outranks a report or a reminder): a marker turn with
# unread mail blocks too, neither stamped from its marker nor floored, and the
# turn end that finally passes stamps from the turn's last marker. It is
# bounded to one block per pending batch by a tick file keyed like the
# heartbeat's, so a model that refuses to poll is nudged once, not in a loop.
#
# What counts as mail: `to` non-empty (a BROADCAST, to == "", never fires this —
# the same demotion rule the sender and the daemon's wake apply), `from` neither
# this handle (self-echo), and the line sitting PAST the read cursor. The cursor is a LINE OFFSET (comm-lib.sh's sot_cursor_offset owns
# the format, including the one-shot conversion of a legacy ts cursor, which
# is NEVER written back from here). Timestamps could not do this job: they are second-resolution
# and every comparison was strictly-greater, so a frame filed in the same second
# as one already read would be announced to nobody while its sender was told it
# had landed. This hook never advances the cursor — only a real comm-poll.sh
# does, which is what keeps "read" an honest word. Any jq failure yields no mail
# and no block: the same fail-open discipline as the rest of the hook, which
# must never be able to wedge a turn.
MAIL_INBOX="$HOME_DIR/inbox/$NAME.jsonl"
# Both counters start at 0 OUTSIDE the gate: on Windows this file often does
# not exist at all (nothing but the frontend files there) and the frontend arm below must
# still run.
mail_total=0; mail_pending=0
# The count and the read run in a SUBSHELL that sources comm-lib.sh, so this
# hook sees exactly the offset comm-poll.sh does (sot_cursor_offset: the ts
# migration, the past-the-end clamp, the cursor's line hash) and takes the same
# shared read lock (sot_inbox_read_lock) — a line a writer is still fsyncing is
# never counted and then cut back. Only newline-terminated lines are counted.
# A busy inbox does NOT block the turn: behind a frozen writer that would loop
# forever. The hook says so on stderr and checks again at the next turn end.
# Any other lock fault is not busy: the count comes from an unlocked read, so
# mail is never hidden behind the fault, and the warning ($lock_warn) prefixes
# every block this hook emits while it lasts (emit_block); with no block to
# carry it, the hook blocks once per fault through its own tick file, keyed by
# handle and session so a session relaunched under the handle is told too, and
# the next clean check removes it so a new fault blocks again.
# A missing library or any failure yields no mail (fail open).
lock_warn=""
fault_tick="$HOME_DIR/state/lock-fault-$(printf '%s' "$NAME.$mail_key" | tr -c 'A-Za-z0-9._-' '_').tick"
if [ -r "$MAIL_INBOX" ]; then
    mail_out="$( ( . "$FE_LIB" >/dev/null 2>&1 || exit 0
        sot_inbox_read_lock "$NAME" || { echo busy; exit 0; }
        mail_pos="$(sot_cursor_offset "$NAME" 2>/dev/null)"
        mail_total="$(sot_inbox_lines "$NAME")"
        mail_pending=0
        if [ "$mail_total" -gt "$mail_pos" ]; then
            mail_pending="$(sed -n "$((mail_pos + 1)),${mail_total}p" "$MAIL_INBOX" 2>/dev/null \
                | jq -Rrs --arg me "$NAME" '[ split("\n")[] | select(length > 0)
                    | (fromjson? // empty) | select(type == "object")
                    | select(((.to // "") != "") and (.from // "") != $me)
                  ] | length' 2>/dev/null || echo 0)"
        fi
        echo "$mail_total $mail_pending"
        printf '%s\n' "${SOT_INBOX_READ_WARNING:-}" ) 2>/dev/null || true )"
    case "$mail_out" in
        busy) echo "comm-status-idle: the inbox for @$NAME is being written; it will be checked again at the next turn end" >&2 ;;
        *)  { read -r mail_total mail_pending; IFS= read -r lock_warn; } <<< "$mail_out" || true
            [ -n "$lock_warn" ] || [ -z "$mail_out" ] || rm -f "${fault_tick:?}"
            case "$mail_total" in ''|*[!0-9]*) mail_total=0 ;; esac
            case "$mail_pending" in ''|*[!0-9]*) mail_pending=0 ;; esac ;;
    esac
fi
if [ "$mail_pending" -gt 0 ]; then
    # ONE block per pending batch, bounded by a tick file keyed by session.
    mail_tick="$HOME_DIR/state/mail-$(printf '%s' "$mail_key" | tr -c 'A-Za-z0-9._-' '_').tick"
    mail_mark="$mail_total"
    if [ "$(cat "$mail_tick" 2>/dev/null || true)" != "$mail_mark" ]; then
        mkdir -p "$HOME_DIR/state" 2>/dev/null || true
        # FAIL OPEN when the tick cannot be recorded. With no tick there is
        # no bound, and a filesystem that refuses this write refuses
        # comm-poll.sh's cursor write too — so the block would return at
        # every turn end with no way for the session to clear it. A missed
        # announcement is acceptable; an inescapable block is not.
        if printf '%s' "$mail_mark" 2>/dev/null > "$mail_tick"; then
            [ -z "$lock_warn" ] || printf '%s' "$lock_warn" 2>/dev/null > "$fault_tick" || true
            emit_block "$(jq -nc --arg n "$NAME" '{
              decision: "block",
              reason: ("New sot-comm mail for @" + $n + " — run comm-poll.sh now, act on it, then end the turn.")
            }')"
            exit 0
        fi
    fi
fi
# A lock fault with no mail block to carry it: ONE block per fault, failing
# open like the mail tick when the tick cannot be recorded. Never inside a
# stop-hook continuation: the fault waits for the next turn end, as a nudge
# does (the loop guard below).
if [ -n "$lock_warn" ] && [ "$(jqget '.stop_hook_active // false')" != "true" ] \
    && [ "$(cat "$fault_tick" 2>/dev/null || true)" != "$lock_warn" ]; then
    mkdir -p "$HOME_DIR/state" 2>/dev/null || true
    if printf '%s' "$lock_warn" 2>/dev/null > "$fault_tick"; then
        print_block "$(jq -nc --arg w "$lock_warn" '{decision: "block", reason: $w}')"
        exit 0
    fi
fi

if [ -n "$marker_state" ]; then
    # Explicit: the marker IS the model's report. `waiting` sets the fact;
    # every other marker clears it (comm-status.sh's declaration reduction).
    [ -x "$STATUS" ] && "$STATUS" "$marker_state" "$marker_summary" >/dev/null 2>&1 || true
    # Every Stop still ends with `stop` (ADR 0044 amendment): it clears
    # `floor` and sets `done` only when floor was user AND neither `question`
    # nor `waiting` is set — the fact the marker just set survives untouched.
    turn_floor

    # ARTIFACT AUDIT EXCEPTION (2026-09-14): the row is already stamped from
    # the marker above -- this only catches a result the closing block named
    # (or produced) but never badged into the nav pane. Loop guard first: a
    # stop-hook continuation never gets a second nudge.
    [ "$(jqget '.stop_hook_active // false')" = "true" ] && exit 0
    AUDITOR="$SELF_DIR/comm-turn-auditor.sh"
    if [ -x "$AUDITOR" ] && [ -n "$tp" ]; then
        findings="$(SOT_AUDITOR_CHECKS=artifact "$AUDITOR" "$NAME" "$tp" 2>/dev/null)"; arc=$?
        if [ "$arc" -eq 0 ] && [ -n "$findings" ]; then
            # Same MSYS2 argv-conversion guard as the general auditor path
            # below: findings is free text and must not reach jq via --arg.
            _findings_file="$(mktemp "${TMPDIR:-/tmp}/sot-comm-idle-marker-findings.XXXXXX" 2>/dev/null)"
            if [ -n "$_findings_file" ] && printf '%s' "$findings" > "$_findings_file" 2>/dev/null; then
                emit_block "$(jq -nc --rawfile f "$_findings_file" '{
                  decision: "block",
                  reason: ("Your closing block names a result that was never surfaced: " + $f + " -- badge it now via the show-result skill (show-result <path>), then end the turn. Your row is already stamped from the marker -- do not write a second sitrep block.")
                }')"
                rm -f "${_findings_file:?}"
                exit 0
            fi
            [ -z "${_findings_file:-}" ] || rm -f -- "${_findings_file:?}" 2>/dev/null
        fi
    fi
    exit 0
fi

# Loop guard: if we are ALREADY in a stop-hook continuation, never re-nudge —
# floor + let the turn end (one nudge per turn, no infinite continue-loop).
[ "$(jqget '.stop_hook_active // false')" = "true" ] && { turn_floor; exit 0; }

# A parked end state without its report. The row shows `blocked` or `done`
# at Stop time only when the MODEL stamped it this turn; on a HUMAN turn
# that owes the matching closing block. One nudge, then the continuation's
# marker stamps the row above. A machine wake (relay, Monitor, notification)
# never nudges: floor and end. A `waiting` fact is NEVER nudged here — it is
# not this turn's word, and a wait carried over from an earlier turn would
# otherwise nudge every short exchange on the row (ADR 0044 amendment, a
# deleted arm); a turn that IS newly waiting still declares SITREP-WAITING:
# and the marker path above sets it.
# sot_registry_read as above (0 a row, 1 no row, 2 unreadable); only a row
# prints anything, so no row and unreadable are both "|".
row_facts="$( ( . "$FE_LIB" >/dev/null 2>&1 || exit 2; sot_registry_read "$NAME" ) | jq -r '(.floor // "") + "|" + (if .question != null then "blocked" elif .done == true then "done" else "" end)' 2>/dev/null)"
[ -n "$row_facts" ] || row_facts="|"
origin="${row_facts%%|*}"; parked="${row_facts#*|}"
stored_origin="$origin"

# TURN ORIGIN CORRECTION (2026-09-15): UserPromptSubmit does not fire for
# every harness-injected wake -- a subagent/peer report or idle notice
# arriving AS a prompt, a cross-session message, this hook's own send-back --
# so $origin above can still be a stale "user" left by the last HUMAN prompt.
# Classify the transcript's own last-prompt-record text the same way
# comm-status-working.sh classifies hook stdin (that script is this check's
# twin -- keep both pattern lists in sync by hand; no shared library, both
# hooks stay standalone by design). $prompt_text is "" when the turn is
# longer than the tail ($h was null above), so this never fires there --
# $origin is left exactly as read, same as before this change. This hook's
# own recorded feedback never lands here (the slice pass skips it), so the
# "Stop hook feedback:" arm now covers only feedback it did not record.
case "$prompt_text" in
    "[SYSTEM NOTIFICATION"*|*"<task-notification>"*|"[relay] from"*|"[sot-comm] "*|\[*:*\]\ *)
        origin=machine ;;
    "Another Claude session sent a message"*|*"<teammate-message"*|*"<agent-message"*|*"<cross-session-message"*|"Stop hook feedback:"*)
        origin=machine ;;
esac
# Correct the registry too, not just this run's decision, so the plain
# turn-end `stop` (reached on every OTHER path below) also floors gray
# rather than blue -- it re-reads `floor` fresh, not this script's $origin.
# Reuses comm-status.sh's own `prompt` event: a machine origin only sets
# `floor`, clearing nothing else, so this corrects provenance without
# touching the question/done/waiting facts already on the row.
if [ "$origin" = machine ] && [ "$stored_origin" != machine ] && [ -x "$STATUS" ]; then
    COMM_STATUS_ORIGIN=machine "$STATUS" prompt >/dev/null 2>&1 || true
fi
case "$parked" in
    blocked|done)
        if [ "$origin" = user ]; then
            case "$parked" in
                blocked) owed='SITREP-QUESTION: <the exact question, one sentence>  then the context needed to answer it cold: what was being done, the options and what follows from each, the default if unanswered, what is irreversible' ;;
                *)       owed='SITREP: <one-line headline>  then the sitrep chain (the sitrep skill): issue in context, diagnosis, design, result with its scale, interpretation, plan' ;;
            esac
            emit_block "$(jq -nc --arg s "$parked" --arg o "$owed" '{
              decision: "block",
              reason: ("Your row ends this turn as `" + $s + "` but the reply carries no closing marker. Write the closing block now, as the last thing in your reply, opening with the marker line:  " + $o + ".  The Stop hook stamps the row from that line (the rest of the marker line is the nav summary). If the state is wrong, run comm-status.sh with the right one and still close with the matching marker.")
            }')"
            exit 0
        fi
        turn_floor; exit 0 ;;
esac

# An EFFORT that ends green with no marker (2026-09-10): the common shape of a
# session that never stamps. One SOFT nudge — the model decides whether the
# turn closed an effort (owes the sitrep block) or was a step in a live
# back-and-forth (owes nothing). Short turns never trip this, whatever they
# did; the thresholds are the effort/exchange line, not a work detector.
if [ "$origin" = user ] && { [ "$turn_tools" -ge "$EFFORT_TOOLS" ] || [ "$turn_secs" -ge "$EFFORT_SECS" ]; }; then
    emit_block "$(jq -nc --arg n "$turn_tools" --arg m "$((turn_secs / 60))" '{
      decision: "block",
      reason: ("This turn ran " + $n + " tool calls over " + $m + " min and ends with no closing marker. IF it CLOSED a work effort (a result landed, a fix shipped, a diagnosis was reached, a decision point arrived), close with the sitrep block now, as the last thing in your reply: a line  SITREP: <one-line headline>  then the chain (issue in context, diagnosis, design, result with its scale, interpretation, plan) in plain words -- no hashes, paths, names, backticks or bullets. IF this turn was a step in a live back-and-forth with the user, end normally: no block is owed. This will not fire again this turn.")
    }')"
    exit 0
fi

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
                emit_block "$(jq -nc --rawfile f "$_findings_file" '{
                  decision: "block",
                  reason: ("Turn-end audit: " + $f + " -- IF a finding is real, act on it now AND clearly RESTATE it for the user: blocked -> restate the exact question you are awaiting (one standalone sentence, as BOTH the comm-status summary and the final line of your reply); waiting -> state plainly what is being monitored and what completion looks like (same two places); artifact -> badge it via the show-result skill. IF a finding is wrong (rhetorical question, artifact already shown, job already done), just end the turn normally. This audit will not re-fire for the same situation.")
                }')"
                rm -f "${_findings_file:?}"
            else
                # Temp file failed -- degrade rather than risk a corrupted
                # --arg on Windows; the legacy grep fallback below still runs.
                [ -z "${_findings_file:-}" ] || rm -f -- "${_findings_file:?}" 2>/dev/null
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
    emit_block "$(jq -nc '{
      decision: "block",
      reason: "Reminder: your last reply contains a question mark, and a plain-text question (not the AskUserQuestion tool) fires no automatic frontend signal. IF you are ending this turn AWAITING THE USER on a blocking question, run  ~/.sot-comm/bin/comm-status.sh blocked \"<the question>\"  now so your row shows red on the frontend. IF the question(s) were rhetorical or already answered, just end the turn normally — this nudge will not fire again this turn."
    }')"
    exit 0
fi

# No question → plain idle floor.
turn_floor
exit 0
