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
# reparented (setsid), logging to /tmp/sotd.log. It also reports
# whether the running daemon was actually STALE vs the binary, so you can see if
# a restart was even needed.
#
# Usage: scripts/restart-backend.sh [--check]
#   --check : report staleness and exit WITHOUT restarting.
#             exit 3 = stale (or none running), exit 0 = current.
set -uo pipefail
PORT="${SOT_TCP_PORT:-18743}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="${SOT_PROJECT_ROOT:-$REPO}"
BIN="$REPO/rust/target/release/sotd"
LOG="${SOT_BACKEND_LOG:-/tmp/sotd.log}"

find_pid() { ps -eo pid,args | grep "[s]otd --tcp 127.0.0.1:$PORT" | grep -v '/bin/bash' | awk '{print $1}' | head -1; }
port_open() { (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null && exec 3>&-; }

[ -x "$BIN" ] || { echo "ERROR: binary not built: $BIN" >&2
    echo "       build it: (cd '$REPO/rust' && cargo build --release -p sot-backend)" >&2; exit 2; }

OLD=$(find_pid)
BIN_MTIME=$(stat -c %Y "$BIN")
if [ -n "$OLD" ]; then
    START_EPOCH=$(( $(date +%s) - $(ps -p "$OLD" -o etimes= | tr -d ' ') ))
    if [ "$BIN_MTIME" -gt "$START_EPOCH" ]; then
        STALE=1
        echo "running daemon pid $OLD is STALE — started $(date -d "@$START_EPOCH" '+%F %T'), binary built $(date -d "@$BIN_MTIME" '+%F %T')"
    else
        STALE=0
        echo "running daemon pid $OLD is CURRENT — started $(date -d "@$START_EPOCH" '+%F %T') >= binary $(date -d "@$BIN_MTIME" '+%F %T')"
    fi
else
    STALE=1
    echo "no daemon currently running on $PORT"
fi

if [ "${1:-}" = "--check" ]; then
    [ "${STALE:-1}" = "1" ] && exit 3 || exit 0
fi

# If sotd is supervised by systemd --user (sotd.service), let systemd own the
# lifecycle: a manual pkill+nohup here would race its Restart=always and double-
# bind $PORT. `systemctl restart` picks up the freshly-built on-disk binary too.
if systemctl --user is-enabled sotd.service >/dev/null 2>&1; then
    echo "sotd is systemd-supervised (sotd.service) — restarting via systemctl --user"
    systemctl --user restart sotd.service
    for _ in $(seq 1 30); do port_open && break; sleep 0.5; done
    NEW=$(find_pid)
    if [ -n "$NEW" ]; then
        echo "backend restarted via systemd: pid $NEW on $PORT (binary built $(date -d "@$BIN_MTIME" '+%F %T'))"
        exit 0
    fi
    echo "ERROR: systemd sotd did not bind $PORT after restart — see: systemctl --user status sotd" >&2; exit 1
fi

# --- legacy path: no systemd supervision, detached-nohup relaunch ---
# Kill the old daemon by EXPLICIT pid (never `pkill -f 'sotd ...'` —
# it matches its own shell's argv and kills the script: classic exit-144).
if [ -n "$OLD" ]; then
    kill "$OLD" 2>/dev/null
    for _ in $(seq 1 20); do port_open || break; sleep 0.5; done
    if kill -0 "$OLD" 2>/dev/null; then echo "force-killing $OLD"; kill -9 "$OLD" 2>/dev/null; sleep 1; fi
fi

# Relaunch detached + reparented (setsid -> own session, outlives this script;
# nohup -> ignore SIGHUP). Parent dies -> daemon reparents to init (ppid 1).
setsid nohup "$BIN" --tcp "127.0.0.1:$PORT" --project-root "$ROOT" --label sot >>"$LOG" 2>&1 &
disown 2>/dev/null || true

for _ in $(seq 1 30); do port_open && break; sleep 0.5; done
NEW=$(find_pid)
if [ -n "$NEW" ]; then
    echo "backend restarted: pid $NEW on $PORT (binary built $(date -d "@$BIN_MTIME" '+%F %T'))"
else
    echo "ERROR: backend did not bind $PORT after restart — see $LOG" >&2; exit 1
fi
