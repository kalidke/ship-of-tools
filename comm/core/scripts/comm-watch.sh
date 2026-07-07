#!/usr/bin/env bash
# comm-watch.sh — the command a harness Monitor runs to WAKE this session on new
# directed fast-comm. Foreground poll loop: one stdout line per new *directed*
# relay frame in your inbox. Arg: $1 = your handle (the joined NAME).
#
#   Monitor command:  comm-watch.sh <handle>
#
# This replaces the fragile hand-pasted multiline jq Monitor body (the handle had
# to be substituted into the loop in two places by hand). Keep the loop here so
# the skill says "arm a Monitor running `comm-watch.sh <handle>`" — one editable
# place, no copy-paste substitution.
#
# WHY POLL, NOT `tail -F`: the inbox lives under $HOME, which is NFS on the Linux
# cohort. `tail -F` relies on inotify, which is unreliable over NFS — it silently
# misses/delays writes (a relay message once surfaced 45 minutes late). Re-opening
# the file every 2s gets NFS close-to-open consistency, so each read sees the
# latest content.
#
# WHAT WAKES vs WHAT IS DROPPED (the jq select):
#   - your own echoes (.from == handle)        -> dropped (don't wake on self)
#   - broadcasts (.to == "")                   -> dropped here, demoted to silent;
#                                                 comm-poll.sh surfaces them on your
#                                                 next natural turn (wake-ups cost a
#                                                 model turn each)
#   - everything else (directed, .to non-empty) -> emitted -> wakes the session
#   - the __selftest__ frame is NOT special-cased and MUST stay emitted: it's
#     from:__selftest__ to:<you> (directed, non-empty .to), so it passes the
#     select naturally. The post-arm wake-proof in sot-session-start RELIES on
#     this Monitor firing on that frame. (comm-poll.sh does the opposite and
#     FILTERS __selftest__ — wake here, ignore there; do not conflate.)
set -uo pipefail

handle="${1:-}"
if [ -z "$handle" ]; then
    echo "usage: comm-watch.sh <handle>" >&2
    exit 2
fi

inbox="$HOME/.sot-comm/inbox/$handle.jsonl"

# Line count that is robust to a missing/unreadable inbox WITHOUT noise: a freshly
# joined handle may not have a file until its first frame lands. `wc -l < missing`
# would make the SHELL (doing the `<` redirect) print "No such file" to stderr
# BEFORE wc's own `2>/dev/null` could suppress it — same redirect-noise class as
# the comm-listen _inject fix. Test readability first; treat absent as 0 lines.
linecount() { [ -r "$inbox" ] && wc -l < "$inbox" 2>/dev/null || echo 0; }

n=$(linecount)
while true; do
    c=$(linecount)
    # File shrank/rotated/recreated — reset to 0 so the next compare re-reads the
    # whole (now-smaller) file from line 1. Resetting to $c instead would skip any
    # lines appended in the SAME poll cycle as the shrink (truncate + append before
    # the next poll => c==n => nothing emitted). Reset-to-0 emits them.
    [ "$c" -lt "$n" ] && n=0
    if [ "$c" -gt "$n" ]; then
        # --arg me passes the handle safely (no string-splice). The `.to // "?"`
        # default makes a legacy line with NO .to key read as non-empty -> wakes
        # (those predate the to-stamp and are treated as directed).
        awk -v s="$n" 'NR>s' "$inbox" | while IFS= read -r l; do
            printf '%s' "$l" | jq -rc --arg me "$handle" \
                'select(.from != $me and ((.to // "?") != "")) | "[relay] from \(.from): \(.msg)"' 2>/dev/null
        done
        n=$c
    fi
    sleep 2
done
