#!/usr/bin/env bash
# comm-poll.sh — show the inbox lines past the read cursor, then advance it.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home

[ -z "$NAME" ] && { echo "Not joined — run comm-join.sh first." >&2; exit 1; }

INBOX="$INBOX_DIR/$NAME.jsonl"
CUR="$READ_DIR/$NAME.cursor"

if [ ! -f "$INBOX" ]; then
    echo "No messages."
    with_lock registry_touch "$NAME" 2>/dev/null || true
    exit 0
fi

# The cursor is a LINE OFFSET (comm-lib.sh's sot_cursor_offset, which migrates a
# legacy ts cursor on first read): every line up to it has been shown. Comparing
# timestamps could not separate two frames filed in the same second, so one of
# them was shown to nobody while its sender was told it had landed.
pos="$(sot_cursor_offset "$NAME")"
total="$(sot_inbox_lines "$NAME")"

count=0
if [ "$total" -gt "$pos" ]; then
    while IFS= read -r line; do
        [ -z "$line" ] && continue
        # A TORN or otherwise unparseable line is SKIPPED, never fatal: under
        # this script's `set -e` a jq failure here exited the whole poll, which
        # froze the cursor and left the handle permanently deaf while its senders
        # kept printing a success line. It is still COUNTED as read (the cursor
        # advances to $total below), so one bad line cannot pin the cursor
        # either.
        printf '%s' "$line" | jq -e 'type == "object"' >/dev/null 2>&1 || continue
        from="$(printf '%s' "$line" | jq -r '.from // ""' 2>/dev/null)"
        # Selftest frames (from:__selftest__) are wake-path proofs injected by
        # comm-listen.sh --selftest; they land in the durable inbox but are NOT
        # real peer messages, so they are not SHOWN. They are still counted as
        # read below: a cursor that stuck behind one re-showed every frame after
        # it, for as long as it sat there. (comm-watch.sh deliberately does the
        # OPPOSITE -- it WAKES on a __selftest__ frame, because that frame is
        # exactly the post-arm wake-proof.)
        [ "$from" = "__selftest__" ] && continue
        printf '[%s] [%s:%s] %s\n' \
            "$(printf '%s' "$line" | jq -r '.ts // ""' 2>/dev/null)" \
            "$from" \
            "$(printf '%s' "$line" | jq -r '.repo // ""' 2>/dev/null)" \
            "$(printf '%s' "$line" | jq -r '.msg // .message // .text // ""' 2>/dev/null)"
        count=$((count + 1))
    done < <(sed -n "$((pos + 1)),${total}p" "$INBOX")
    # Advancing the cursor is what "read" MEANS here: nothing else writes this
    # file, which is why the end-of-turn hook can announce pending mail without
    # ever being able to mark it read itself.
    printf '%s' "$total" > "$CUR"
fi
[ "$count" -eq 0 ] && echo "No new messages."
with_lock registry_touch "$NAME" 2>/dev/null || true
