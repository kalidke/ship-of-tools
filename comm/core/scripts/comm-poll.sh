#!/usr/bin/env bash
# comm-poll.sh — show inbox messages newer than the read cursor, then advance it.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home

[ -z "$NAME" ] && { echo "Not joined — run comm-join.sh first." >&2; exit 1; }

INBOX="$INBOX_DIR/$NAME.jsonl"
CUR="$READ_DIR/$NAME.cursor"
last="$(cat "$CUR" 2>/dev/null || echo "")"

if [ ! -f "$INBOX" ]; then
    echo "No messages."
    with_lock registry_touch "$NAME" 2>/dev/null || true
    exit 0
fi

newest=""; count=0
while IFS= read -r line; do
    [ -z "$line" ] && continue
    from="$(printf '%s' "$line" | jq -r '.from')"
    # Selftest frames (from:__selftest__) are wake-path proofs injected by
    # comm-listen.sh --selftest; they land in the durable inbox but are NOT real
    # peer messages. Skip them here so catch-up doesn't surface phantom "missed
    # messages". (comm-watch.sh deliberately does the OPPOSITE — it WAKES on a
    # __selftest__ frame, because that frame is exactly the post-arm wake-proof.)
    [ "$from" = "__selftest__" ] && continue
    ts="$(printf '%s' "$line" | jq -r '.ts')"
    if [ -z "$last" ] || [[ "$ts" > "$last" ]]; then
        repo="$(printf '%s' "$line" | jq -r '.repo')"
        msg="$(printf '%s' "$line" | jq -r '.msg')"
        echo "[$ts] [$from:$repo] $msg"
        newest="$ts"; count=$((count + 1))
    fi
done < "$INBOX"

[ -n "$newest" ] && printf '%s' "$newest" > "$CUR"
[ "$count" -eq 0 ] && echo "No new messages."
with_lock registry_touch "$NAME" 2>/dev/null || true
