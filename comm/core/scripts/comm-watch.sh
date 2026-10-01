#!/usr/bin/env bash
# comm-watch.sh — the command a harness Monitor runs to WAKE this session on new
# directed fast-comm. Foreground poll loop: one stdout line per new *directed*
# relay frame in any of your inboxes. Arg: $1 = your handle (the joined NAME).
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
#
# LIVENESS MARKER: comm-session-start.sh's survival check needs to tell a
# live Monitor from a dead one. Linux does this with `pgrep` against the
# process table directly — no marker needed there. git-bash on Windows has
# no reliable pgrep, so this script instead writes ITS OWN pid ($$) to
# state/<handle>.watch ONCE at startup (plus the arming session's id on a
# second line — see the write below); the survival check reads that pid
# back and asks the OS (`kill -0`) whether it's still alive. This is
# deliberately NOT an age/heartbeat heuristic (Codex review finding 4: a
# "touched within the last N seconds" test misreads BOTH ways — a killed
# watcher can still look alive inside the window, and a live one can look
# dead after a suspend/GC pause or a slow poll cycle) — a stale PID in the
# marker only misfires in the rare window after that exact PID is reused by
# an unrelated process, the same accepted-and-documented limitation every
# PID-based liveness check carries (POSIX has no stronger primitive).
set -uo pipefail

handle="${1:-}"
if [ -z "$handle" ]; then
    echo "usage: comm-watch.sh <handle>" >&2
    exit 2
fi

# comm-lib.sh, for the platform branch and the home derivation. This script used
# to mirror both by hand to stay dependency-free — and mirroring is exactly how it
# came to watch a different file from the one comm-poll.sh read, for as long as
# nobody compared the two copies. It sits beside this script both in the checkout
# and in the flat ~/.sot-comm/bin the installer deploys, so one path resolves in
# both. FATAL if it will not load: every other comm verb hard-sources it, and a
# watcher that quietly polls the wrong file is the defect being fixed here.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=comm-lib.sh
source "$SCRIPT_DIR/comm-lib.sh" 2>/dev/null || {
    echo "comm-watch.sh: cannot load $SCRIPT_DIR/comm-lib.sh — refusing to watch, because the files to poll are derived there" >&2
    exit 3
}

# The per-handle file is where a directed send lands ($INBOX_DIR honours
# $SOT_COMM_HOME because the library derives it, not a mirrored line). Its
# filter: `.to // "?"` defaults a legacy line with NO .to key to non-empty ->
# wakes (those predate the to-stamp and are treated as directed), and the
# message is under `.msg`.
#
# The filter consults no read cursor, and this script must never write one: a
# watcher wakes on frames that arrive AFTER it is armed, while the cursor means
# "already shown to the model" — arming against the cursor would replay every
# unread frame as a wake, and advancing it here would mark mail read that nobody
# has seen.
sources=("$INBOX_DIR/$handle.jsonl")
filters=('select(.from != $me and ((.to // "?") != "")) | "[relay] from \(.from): \(.msg)"')

marker="$COMM_HOME/state/$handle.watch"
mkdir -p "$(dirname "$marker")" 2>/dev/null || true
# Line 1: this watcher's pid (liveness). Line 2: the claude session that
# armed it (identity, 2026-09-10) — a Monitor's watcher inherits the
# session's env, so this names the ONLY session its wake can ever reach. A
# watcher whose session is gone but whose process is not (the killed
# capsule on a converged box; the pane-less restart on a shared host) used
# to pass the survival check on liveness alone and leave the NEW session
# deaf while it believed itself live — three boxes, three field reports.
printf '%s\n%s\n' "$$" "${CLAUDE_CODE_SESSION_ID:-}" > "$marker" 2>/dev/null || true

# Where each source stood when this watcher was armed: everything already on disk
# belongs to the session's past (comm-poll.sh catches that up), so only lines
# appended from here on wake anybody.
counts=()
for src in "${sources[@]}"; do counts+=("$(sot_file_lines "$src")"); done
while true; do
    # A Monitor whose harness expired still leaves this poll loop running
    # forever otherwise (45 orphans observed on one box) -- $PPID is fixed at
    # startup and is never live-updated by bash on reparenting, so this still
    # correctly reads as gone after the original parent exits.
    kill -0 "$PPID" 2>/dev/null || exit 0
    for i in "${!sources[@]}"; do
        # The per-handle inbox is counted and read under the shared read lock
        # (comm-lib.sh, sot_inbox_read_lock), so a writer's in-flight line is
        # never counted and then cut back; a busy inbox is checked again at the
        # next tick, and any other lock fault goes to stderr (this watch's log)
        # once, not at every tick, and the inbox is read unlocked. Every read
        # is `NR>counted && NR<=c`: only newline-terminated lines, never a dead
        # writer's partial tail.
        locked=0
        case "${sources[$i]}" in
            "$INBOX_DIR/"*) sot_inbox_read_lock "$handle" || continue; locked=1
                sot_inbox_read_warning_log "$handle" ;;
        esac
        c=$(sot_file_lines "${sources[$i]}")
        # File shrank/rotated/recreated — reset to 0 so the next compare re-reads the
        # whole (now-smaller) file from line 1. Resetting to $c instead would skip any
        # lines appended in the SAME poll cycle as the shrink (truncate + append before
        # the next poll => c==n => nothing emitted). Reset-to-0 emits them.
        [ "$c" -lt "${counts[$i]}" ] && counts[$i]=0
        if [ "$c" -gt "${counts[$i]}" ]; then
            # --arg me passes the handle safely (no string-splice).
            awk -v s="${counts[$i]}" -v e="$c" 'NR>s && NR<=e' "${sources[$i]}" | while IFS= read -r l; do
                printf '%s' "$l" | jq -rc --arg me "$handle" "${filters[$i]}" 2>/dev/null
            done
            counts[$i]=$c
        fi
        [ "$locked" -eq 0 ] || sot_inbox_read_unlock
    done
    sleep 2
done
