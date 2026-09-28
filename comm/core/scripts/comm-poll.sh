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
# TWO inboxes on Windows. There is no listener there, so nothing writes the
# per-handle file — the frontend files every inbound frame into its own
# fe-inbox.jsonl, which is SHARED by every handle on the box. Reading only the
# per-handle one left a Windows session deaf to every message from another box
# while this script printed "No new messages" (field report, 2026-09-27).
# comm-lib.sh owns the platform branch, the `to == me` admission rule and the
# `.text` -> `.msg` rewrite, so everything below stays single-schema and never
# learns which file a line came from. Both cursors advance HERE and only here:
# advancing a cursor is what "read" MEANS, which is why the end-of-turn hook can
# announce pending mail without ever being able to mark it read itself.
FE_INBOX="$(sot_fe_inbox_path)"
FE_CUR="$READ_DIR/$NAME.fe.cursor"

if [ ! -f "$INBOX" ] && { [ -z "$FE_INBOX" ] || [ ! -f "$FE_INBOX" ]; }; then
    echo "No messages."
    with_lock registry_touch "$NAME" 2>/dev/null || true
    exit 0
fi

count=0
# show_stream — print the SHOWABLE lines of one inbox stream, read on stdin.
# Called with a process substitution, never a pipe, so $count is this shell's.
show_stream() {
    local line from
    while IFS= read -r line; do
        [ -z "$line" ] && continue
        # A TORN or otherwise unparseable line is SKIPPED, never fatal: under
        # this script's `set -e` a jq failure here exited the whole poll, which
        # froze the cursor and left the handle permanently deaf while its senders
        # kept printing a success line. It is still COUNTED as read (the cursor
        # advances to the total below), so one bad line cannot pin the cursor
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
    done
}

# The cursor is a LINE OFFSET (comm-lib.sh's sot_cursor_offset, which migrates a
# legacy ts cursor on first read): every line up to it has been shown. Comparing
# timestamps could not separate two frames filed in the same second, so one of
# them was shown to nobody while its sender was told it had landed.
pos="$(sot_cursor_offset "$NAME")"
total="$(sot_inbox_lines "$NAME")"

if [ -f "$INBOX" ] && [ "$total" -gt "$pos" ]; then
    show_stream < <(sed -n "$((pos + 1)),${total}p" "$INBOX")
    printf '%s' "$total" > "$CUR"
fi

if [ -n "$FE_INBOX" ] && [ -r "$FE_INBOX" ]; then
    # The total is read BEFORE the lines are shown, so a frame appended in
    # between is shown by the NEXT poll instead of being skipped by this one:
    # showing a frame twice is tolerable where dropping one is not.
    fe_total="$(sot_fe_inbox_lines)"
    show_stream < <(sot_fe_unread_lines "$NAME")
    printf '%s' "$fe_total" > "$FE_CUR"
fi

[ "$count" -eq 0 ] && echo "No new messages."
with_lock registry_touch "$NAME" 2>/dev/null || true
