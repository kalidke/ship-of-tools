#!/usr/bin/env bash
# bus.sh — the ops-sidecar git bus: durable cross-OS Claude-to-Claude notes,
# for whatever the live relay never delivered (no listener up on the other
# side, or a message sent while a machine was down).
#
# Usage:
#   bus.sh note "<text>"   append an entry to THIS side's from-<os>.md,
#                          commit (scoped to just that file), and push
#                          (unless other commits are already pending on the
#                          branch — see below). Prints the commit SHA.
#   bus.sh sync [--count]  pull/rebase, then print new entries from the
#                          OTHER side's from-<os>.md since the last-synced
#                          position, advancing the cursor past them. With
#                          --count: print ONLY the number of unseen entries
#                          and NEVER advance the cursor — a pure peek a
#                          caller can fold into a one-line verdict without
#                          silently marking anything as read.
#
# Neither verb is read-only: `note` writes the bus file, commits, and
# usually pushes; `sync` always does a real `git pull --rebase` against the
# sidecar (a write to that checkout's ref/working tree) even with --count,
# and a plain `sync` also writes the cursor file.
#
# The bus lives in the PRIVATE ops sidecar repo (relocated pre-public-flip,
# ADR 0030 §7), resolved as $SOT_OPS_DIR or the sibling ../ship-of-tools-ops
# next to the ACTIVE product checkout (the repo the caller's cwd is actually
# in — a worktree or subdirectory resolves against ITS OWN toplevel, never
# this script's install location) — ONE resolution, used by both verbs.
#
# Entry format (see <ops>/claude-bus/README.md): each entry is
# `## TIMESTAMP — host · user\n\nBODY\n\n---\n`; entries are delimited by the
# literal `---` separator line, NOT by "## " (a note's own body text may
# legitimately contain a markdown heading). The sync cursor is an APPEND
# POSITION (a line number into from-<other>.md), not a timestamp — a
# timestamp has only minute resolution and can't tell apart two notes
# appended in the same minute; a line-number cursor can't skip or duplicate
# across that race, and a malformed/stale value (past EOF — the file was
# reset) is simply treated as 0 rather than silently wedging.
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

_side() {
    case "$(uname -s 2>/dev/null || true)" in
        Linux|Darwin) echo "linux" ;;
        *)            echo "windows" ;;
    esac
}

# _ops_dir — resolve relative to the ACTIVE product checkout (the caller's
# cwd), never this script's own install directory (Codex review finding 12:
# the old version used `git -C "$SCRIPT_DIR"`, which — once installed to
# ~/.sot-comm/bin — is not inside any product repo at all, and even from
# source resolves the WRONG checkout for a caller working in a worktree or a
# subdirectory of one). Mirrors comm-context.sh's own RAW_ROOT resolution
# exactly: `git rev-parse --show-toplevel` from cwd, falling back to cwd
# itself outside any repo.
_ops_dir() {
    if [ -n "${SOT_OPS_DIR:-}" ]; then
        printf '%s\n' "$SOT_OPS_DIR"
        return 0
    fi
    local root
    root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
    printf '%s\n' "$(dirname "$root")/ship-of-tools-ops"
}

# _ops_valid DIR — true iff DIR is a usable git checkout (a worktree's
# `.git` is a FILE, not a directory, so `[ -d "$DIR/.git" ]` — the old
# check — wrongly rejects one; Codex review finding 12).
_ops_valid() {
    git -C "$1" rev-parse --is-inside-work-tree >/dev/null 2>&1
}

SUB="${1:-}"; [ $# -gt 0 ] && shift || true

case "$SUB" in
    note)
        TEXT="${1:-}"
        [ -n "$TEXT" ] || { echo "usage: bus.sh note \"<text>\"" >&2; exit 2; }
        OPS="$(_ops_dir)"
        _ops_valid "$OPS" || { echo "bus.sh: ops sidecar not found at $OPS (set \$SOT_OPS_DIR?)" >&2; exit 1; }
        SIDE="$(_side)"
        RELFILE="claude-bus/from-$SIDE.md"
        ABS_FILE="$OPS/$RELFILE"
        [ -f "$ABS_FILE" ] || { echo "bus.sh: $ABS_FILE does not exist — check the ops sidecar's claude-bus layout" >&2; exit 1; }
        TS="$(date -u +%Y-%m-%dT%H:%MZ)"
        HOST_NAME="$(hostname 2>/dev/null || echo unknown)"
        USER_NAME="$(whoami 2>/dev/null || echo unknown)"
        {
            printf '\n## %s — %s · %s\n\n%s\n\n---\n' "$TS" "$HOST_NAME" "$USER_NAME" "$TEXT"
        } >> "$ABS_FILE"

        # Scoped commit (Codex review finding 10): `git commit -- <pathspec>`
        # commits ONLY this file's current content, regardless of what else
        # might already be staged in the sidecar's index — never an
        # unqualified `git add -A && git commit`, which would publish
        # unrelated in-progress work sitting in that shared checkout.
        if ! git -C "$OPS" commit -q -m "bus: note from $SIDE" -- "$RELFILE"; then
            echo "bus.sh: commit failed — see git output above; the entry is written to disk but not committed" >&2
            exit 1
        fi
        SHA="$(git -C "$OPS" rev-parse --short HEAD)"

        # Refuse to publish commits we didn't just make (Codex review finding
        # 10): if the branch already had commits ahead of its upstream
        # BEFORE ours (checked via HEAD^, i.e. what HEAD was before this
        # commit), a push would publish those too — possibly someone else's
        # in-progress work in this shared sidecar checkout. `|| echo 0` on a
        # missing/unconfigured upstream treats it as "nothing pending" —
        # the same as today's no-upstream-checking behavior, not a new gap.
        PENDING_BEFORE="$(git -C "$OPS" rev-list --count '@{u}..HEAD^' 2>/dev/null || echo 0)"
        if [ "${PENDING_BEFORE:-0}" != "0" ]; then
            echo "bus.sh: note committed locally as $SHA, but NOT pushed — $PENDING_BEFORE other commit(s) were already pending on this branch ahead of upstream; review and push manually" >&2
            exit 0
        fi
        if ! git -C "$OPS" push -q; then
            echo "bus.sh: commit $SHA succeeded but push failed — see git output above" >&2
            exit 1
        fi
        echo "bus.sh: note committed and pushed as $SHA on $SIDE"
        ;;
    sync)
        COUNT_ONLY=0
        [ "${1:-}" = "--count" ] && COUNT_ONLY=1
        OPS="$(_ops_dir)"
        if ! _ops_valid "$OPS"; then
            if [ "$COUNT_ONLY" = 1 ]; then echo "n/a"; else echo "bus.sh: ops sidecar not found at $OPS (set \$SOT_OPS_DIR?)" >&2; fi
            exit 0
        fi
        if ! git -C "$OPS" pull -q --rebase 2>/dev/null; then
            if [ "$COUNT_ONLY" = 1 ]; then echo "n/a"; else echo "bus.sh: pull failed in $OPS — resolve manually (uncommitted changes? network?)" >&2; fi
            exit 1
        fi
        SIDE="$(_side)"
        OTHER="linux"; [ "$SIDE" = "linux" ] && OTHER="windows"
        SRC="$OPS/claude-bus/from-$OTHER.md"
        CURSOR="$OPS/claude-bus/.cursor-$SIDE"

        if [ ! -f "$SRC" ]; then
            [ "$COUNT_ONLY" = 1 ] && echo 0 || echo "bus is quiet — $SRC does not exist yet"
            exit 0
        fi

        TOTAL_LINES="$(wc -l < "$SRC" 2>/dev/null || echo 0)"
        RAW_CURSOR="$(cat "$CURSOR" 2>/dev/null || true)"
        LAST_LINE=0
        # A legacy (timestamp-format) or otherwise malformed cursor, or one
        # pointing PAST the current EOF (the file was reset/rewritten), is
        # invalid evidence — treat as 0 (epoch) rather than refusing to sync
        # or silently wedging on it forever (Codex review finding 13).
        if [[ "$RAW_CURSOR" =~ ^[0-9]+$ ]] && [ "$RAW_CURSOR" -le "$TOTAL_LINES" ]; then
            LAST_LINE="$RAW_CURSOR"
        fi

        if [ "$LAST_LINE" -ge "$TOTAL_LINES" ]; then
            [ "$COUNT_ONLY" = 1 ] && echo 0 || echo "bus is quiet — no new entries from $OTHER"
            exit 0
        fi

        # Parse only the NEW region into whole entries, delimited by a lone
        # "---" line (never by "## ", which a note's own body could contain
        # — Codex review finding 13). A trailing PARTIAL entry (no closing
        # --- yet, e.g. a concurrent writer mid-append) is left for next
        # time: NEW_LAST_LINE only ever advances to the last COMPLETE
        # entry's closing line, so nothing is shown — or acknowledged —
        # before it's actually whole.
        NEW_LAST_LINE="$LAST_LINE"
        ENTRIES=""; COUNT=0; entry_buf=""; line_no="$LAST_LINE"
        while IFS= read -r line; do
            line_no=$((line_no + 1))
            if [ "$line" = "---" ]; then
                if [ -n "$entry_buf" ]; then
                    COUNT=$((COUNT + 1))
                    ENTRIES="${ENTRIES}${entry_buf}"$'---\n'
                fi
                entry_buf=""
                NEW_LAST_LINE="$line_no"
            else
                entry_buf="${entry_buf}${line}"$'\n'
            fi
        done < <(sed -n "$((LAST_LINE + 1)),\$p" "$SRC")

        if [ "$COUNT_ONLY" = 1 ]; then
            echo "$COUNT"
        elif [ "$COUNT" -eq 0 ]; then
            echo "bus is quiet — no new entries from $OTHER"
        else
            printf '%s' "$ENTRIES"
        fi

        # Only a REAL display (never --count) advances the cursor (Codex
        # review finding 9): counting must never consume unseen entries —
        # the old version advanced on --count too, permanently hiding
        # whatever a bootstrap verdict line merely counted but never showed
        # anyone.
        if [ "$COUNT_ONLY" = 0 ] && [ "$NEW_LAST_LINE" -gt "$LAST_LINE" ]; then
            printf '%s' "$NEW_LAST_LINE" > "$CURSOR"
        fi
        exit 0
        ;;
    *)
        echo "usage: bus.sh {note \"<text>\" | sync [--count]}" >&2
        exit 2
        ;;
esac
