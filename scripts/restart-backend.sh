#!/usr/bin/env bash
# restart-backend.sh — load the freshly-built sotd binary NOW.
#
# The backend daemon is standalone (reparented to init; NO supervisor), so a
# `cargo build` does NOT restart it — the running process keeps serving the OLD
# binary until something kills + relaunches it. Worse, scripts/launch-sot.sh
# deliberately LEAVES a running backend alone ("don't disrupt a live session"),
# so a normal FE launch won't pick up a backend fix either. That gap once let a
# pane-derived work-state fix sit built-but-unloaded for ~17h — the daemon had
# been running since before the fix was committed — surfacing to the user as
# "agents show idle when they're actually working". This script is the canonical
# "load the latest binary now".
#
# It kills the running daemon by EXPLICIT pid (never `pkill -f`, which self-
# matches this very shell) and relaunches the on-disk binary detached +
# reparented (setsid), logging to dev/output/sotd-restart.<host>.log in this
# checkout (one writer per file: the home and so the checkout are shared between
# hosts; the daemon also keeps its own private log under its state dir). The
# daemon identity is the per-user socket derived from `--label sot`, not a
# machine-wide TCP port.
# It also reports whether the running daemon was actually STALE vs the binary,
# so you can see if a restart was even needed.
#
# Usage: scripts/restart-backend.sh [--check]
#   --check : report staleness and exit WITHOUT restarting.
#             exit 3 = stale (or none running), exit 0 = current.
set -uo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="${SOT_PROJECT_ROOT:-$REPO}"
BIN="$REPO/rust/target/release/sotd"
LOG="${SOT_BACKEND_LOG:-$REPO/dev/output/sotd-restart.$(uname -n).log}"
LABEL="${SOT_BACKEND_LABEL:-sot}"
SOCKET="${SOT_SOCKET:-}"

[ -x "$BIN" ] || { echo "ERROR: binary not built: $BIN" >&2
    echo "       build it: (cd '$REPO/rust' && cargo build --release -p sot-backend)" >&2; exit 2; }

if [ -z "$SOCKET" ]; then
    SOCKET="$("$BIN" session-socket-path "$LABEL")" || exit 2
fi

# This account's daemon only (ADR 0049, User isolation: another user's `sotd --label sot` is never judged, killed or
# reported as this one): its own processes, the one started on exactly this socket before one started by the label.
find_pid() {
    ps -u "$(id -u)" -o pid=,args= | awk -v sock="$SOCKET" -v label="$LABEL" '
        $0 ~ /[s]otd/ && $0 ~ "--socket " sock { print $1; found = 1; exit }
        $0 ~ /[s]otd/ && $0 ~ "--label " label && first == "" { first = $1 }
        END { if (!found && first != "") print first }
    '
}
# The socket answers this OS account: sotd's own bridge connects only to a socket in a folder private to this account
# and, its input empty, closes again at once.
socket_open() {
    [ -S "$SOCKET" ] || return 1
    "$BIN" stdio-bridge --endpoint "unix:$SOCKET" </dev/null >/dev/null 2>&1
}

OLD=$(find_pid)
BIN_MTIME=$(stat -c %Y "$BIN")
if [ -n "$OLD" ]; then
    if ! socket_open; then
        STALE=1
        echo "running daemon pid $OLD has no socket at $SOCKET"
    else
        START_EPOCH=$(( $(date +%s) - $(ps -p "$OLD" -o etimes= | tr -d ' ') ))
        if [ "$BIN_MTIME" -gt "$START_EPOCH" ]; then
            STALE=1
            echo "running daemon pid $OLD is STALE — started $(date -d "@$START_EPOCH" '+%F %T'), binary built $(date -d "@$BIN_MTIME" '+%F %T')"
        else
            STALE=0
            echo "running daemon pid $OLD is CURRENT — started $(date -d "@$START_EPOCH" '+%F %T') >= binary $(date -d "@$BIN_MTIME" '+%F %T')"
        fi
    fi
else
    STALE=1
    echo "no daemon currently running for $SOCKET"
fi

if [ "${1:-}" = "--check" ]; then
    [ "${STALE:-1}" = "1" ] && exit 3 || exit 0
fi

# If sotd is supervised by systemd --user (sotd.service), let systemd own the
# lifecycle: a manual kill+nohup here would race the unit's own restart policy.
# `systemctl restart` picks up the freshly-built on-disk binary too.
if systemctl --user is-enabled sotd.service >/dev/null 2>&1; then
    echo "sotd is systemd-supervised (sotd.service) — restarting via systemctl --user"
    systemctl --user restart sotd.service
    for _ in $(seq 1 30); do socket_open && break; sleep 0.5; done
    NEW=$(find_pid)
    if [ -n "$NEW" ] && socket_open; then
        echo "backend restarted via systemd: pid $NEW on $SOCKET (binary built $(date -d "@$BIN_MTIME" '+%F %T'))"
        exit 0
    fi
    echo "ERROR: systemd sotd did not bind $SOCKET after restart — reinstall the socket-based unit or see: systemctl --user status sotd" >&2; exit 1
fi

# --- legacy path: no systemd supervision, detached-nohup relaunch ---
# Kill the old daemon by EXPLICIT pid (never `pkill -f 'sotd ...'` —
# it matches its own shell's argv and kills the script: classic exit-144).
if [ -n "$OLD" ]; then
    kill "$OLD" 2>/dev/null
    for _ in $(seq 1 20); do kill -0 "$OLD" 2>/dev/null || break; sleep 0.5; done
    if kill -0 "$OLD" 2>/dev/null; then echo "force-killing $OLD"; kill -9 "$OLD" 2>/dev/null; sleep 1; fi
fi

# Relaunch detached + reparented (setsid -> own session, outlives this script;
# nohup -> ignore SIGHUP). Parent dies -> daemon reparents to init (ppid 1).
mkdir -p "$(dirname "$LOG")"
( umask 077 && : >>"$LOG" )   # private to this user even if it is new
setsid nohup "$BIN" --project-root "$ROOT" --label "$LABEL" >>"$LOG" 2>&1 &
disown 2>/dev/null || true

for _ in $(seq 1 30); do socket_open && break; sleep 0.5; done
NEW=$(find_pid)
if [ -n "$NEW" ] && socket_open; then
    echo "backend restarted: pid $NEW on $SOCKET (binary built $(date -d "@$BIN_MTIME" '+%F %T'))"
else
    echo "ERROR: backend did not bind $SOCKET after restart — see $LOG" >&2; exit 1
fi
