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
#
# WINDOWS SOURCE: there is no per-handle inbox there — the native frontend
# files every inbound relay frame straight into fe-inbox.jsonl (mirrors
# gpu.rs::sot_state_dir(): `%LOCALAPPDATA%\sot` on Windows, else
# `${XDG_STATE_HOME:-$HOME/.local/state}/sot`), and that file is shared by
# every session on the host — including traffic addressed to a SIBLING
# handle (the `to` field is advisory, not enforced routing; the daemon
# broadcasts to every connection). So the Windows wake filter checks `to`
# against OUR exact handle always, and ALSO the bare `win-fe` family label
# only when THIS handle is itself part of that family (starts with
# `win-fe`) — Codex review finding 7: a plain non-FE `<repo>-<host>`
# Windows capsule must wake only on `to:<me>`, never on FE-family
# broadcasts meant for the frontend driver. The frame carries the message
# under `.text` (the raw `agent.message` payload), not `.msg`
# (comm-relay.sh bridge's transformed field, Linux-only).
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

_sot_comm_watch_is_windows() {
    case "${OS:-}" in Windows_NT) return 0 ;; esac
    case "${OSTYPE:-}" in msys*|cygwin*|win32) return 0 ;; esac
    case "$(uname -s 2>/dev/null || true)" in MINGW*|MSYS*|CYGWIN*) return 0 ;; esac
    return 1
}

if _sot_comm_watch_is_windows; then
    inbox="${LOCALAPPDATA:-${XDG_STATE_HOME:-$HOME/.local/state}}/sot/fe-inbox.jsonl"
    case "$handle" in
        win-fe*) wake_filter='select(.from != $me and ((.to // "") == $me or (.to // "") == "win-fe")) | "[relay] from \(.from): \(.text)"' ;;
        *)       wake_filter='select(.from != $me and (.to // "") == $me) | "[relay] from \(.from): \(.text)"' ;;
    esac
else
    # Honor $SOT_COMM_HOME (Codex review finding 8): a capsule with a
    # non-default comm home must watch that home's inbox, not always
    # $HOME/.sot-comm — comm-lib.sh's own COMM_HOME derives it the same way,
    # but this script stays dependency-free (no `source comm-lib.sh`) so it
    # mirrors just that one line rather than pulling the whole library in.
    inbox="${SOT_COMM_HOME:-$HOME/.sot-comm}/inbox/$handle.jsonl"
    # `.to // "?"` defaults a legacy line with NO .to key to non-empty -> wakes
    # (those predate the to-stamp and are treated as directed).
    wake_filter='select(.from != $me and ((.to // "?") != "")) | "[relay] from \(.from): \(.msg)"'
fi

marker="${SOT_COMM_HOME:-$HOME/.sot-comm}/state/$handle.watch"
mkdir -p "$(dirname "$marker")" 2>/dev/null || true
# Line 1: this watcher's pid (liveness). Line 2: the claude session that
# armed it (identity, 2026-09-10) — a Monitor's watcher inherits the
# session's env, so this names the ONLY session its wake can ever reach. A
# watcher whose session is gone but whose process is not (the killed
# capsule on a converged box; the pane-less restart on a shared host) used
# to pass the survival check on liveness alone and leave the NEW session
# deaf while it believed itself live — three boxes, three field reports.
printf '%s\n%s\n' "$$" "${CLAUDE_CODE_SESSION_ID:-}" > "$marker" 2>/dev/null || true

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
        # --arg me passes the handle safely (no string-splice).
        awk -v s="$n" 'NR>s' "$inbox" | while IFS= read -r l; do
            printf '%s' "$l" | jq -rc --arg me "$handle" "$wake_filter" 2>/dev/null
        done
        n=$c
    fi
    sleep 2
done
