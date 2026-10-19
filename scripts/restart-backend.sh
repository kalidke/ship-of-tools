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
# reparented (a session of its own), capturing its stdout and stderr in dev/output/sotd-restart.<host>.log in this
# checkout (one writer per file: the home and so the checkout are shared between
# hosts). The daemon's log lines go only to its own private log, `sotd log-path`;
# the capture file holds its stderr, which says where that log is. The
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

# sot_socket_open: the launch scripts' one probe of this account's socket (ADR 0049, User isolation).
. "$REPO/scripts/lib/sot-daemon.sh" || exit 2

[ -x "$BIN" ] || { echo "ERROR: binary not built: $BIN" >&2
    echo "       build it: (cd '$REPO/rust' && cargo build --release -p sot-backend)" >&2; exit 2; }

if [ -z "$SOCKET" ]; then
    SOCKET="$("$BIN" session-socket-path "$LABEL")" || exit 2
fi

# This account's daemon only (ADR 0049, User isolation: another user's `sotd --label sot` is never judged, killed or
# reported as this one): the one on exactly this socket, else the first started with exactly this label
# (sot_daemon_pid, lib/sot-daemon.sh).
find_pid() { sot_daemon_pid "$SOCKET" "$LABEL"; }

# The times, read and printed with GNU's or BSD's (macOS's) tools: the binary's mtime by stat -c or -f, the daemon's
# age by ps's portable etime ([[dd-]hh:]mm:ss), and a time printed by date -d @ or -r.
age_of() {  # PID — its seconds since it started
    ps -p "$1" -o etime= 2>/dev/null | awk '{ t = $1; d = 0; if (i = index(t, "-")) { d = substr(t, 1, i - 1); t = substr(t, i + 1) }
        n = split(t, f, ":"); s = 0; for (k = 1; k <= n; k++) s = s * 60 + f[k]; print d * 86400 + s }'
}
at() { date -d "@$1" '+%F %T' 2>/dev/null || date -r "$1" '+%F %T'; }

OLD=$(find_pid)
BIN_MTIME=$(stat -c %Y "$BIN" 2>/dev/null || stat -f %m "$BIN")
if [ -n "$OLD" ]; then
    if ! sot_socket_open "$BIN" "$SOCKET"; then
        STALE=1
        echo "running daemon pid $OLD has no socket at $SOCKET"
    else
        START_EPOCH=$(( $(date +%s) - $(age_of "$OLD") ))
        if [ "$BIN_MTIME" -gt "$START_EPOCH" ]; then
            STALE=1
            echo "running daemon pid $OLD is STALE — started $(at "$START_EPOCH"), binary built $(at "$BIN_MTIME")"
        else
            STALE=0
            echo "running daemon pid $OLD is CURRENT — started $(at "$START_EPOCH") >= binary $(at "$BIN_MTIME")"
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
# A unit in its auto-restart window is `activating`, not active, and is still systemd's.
case "$(systemctl --user show -p ActiveState --value sotd.service 2>/dev/null)" in active|activating|reloading) UNIT_LIVE=1 ;; *) UNIT_LIVE=0 ;; esac
if [ "$UNIT_LIVE" = 1 ]; then
    echo "sotd is systemd-supervised (sotd.service) — restarting via systemctl --user"
    systemctl --user restart sotd.service
    for _ in $(seq 1 30); do sot_socket_open "$BIN" "$SOCKET" && break; sleep 0.5; done
    NEW=$(find_pid)
    if [ -n "$NEW" ] && sot_socket_open "$BIN" "$SOCKET"; then
        echo "backend restarted via systemd: pid $NEW on $SOCKET (binary built $(at "$BIN_MTIME"))"
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

# Relaunch detached + reparented: a session of its own (no controlling terminal, so neither a closed terminal nor a
# Ctrl-C in it reaches the daemon), SIGHUP ignored (nohup), and init its parent once this script exits. The session is
# perl's POSIX::setsid, as macOS ships no setsid(1); setsid() refuses a process group leader, which an interactive
# shell makes of a background job, so perl first forks one that is not.
mkdir -p "$(dirname "$LOG")"
( umask 077 && : >>"$LOG" )   # private to this user even if it is new
nohup perl -MPOSIX -e 'if (getpgrp() == $$) { my $p = fork; defined $p or die "fork: $!\n"; exit 0 if $p }
    POSIX::setsid(); exec { $ARGV[0] } @ARGV or die "exec $ARGV[0]: $!\n"' \
    "$BIN" --project-root "$ROOT" --label "$LABEL" >>"$LOG" 2>&1 </dev/null &
disown 2>/dev/null || true

for _ in $(seq 1 30); do sot_socket_open "$BIN" "$SOCKET" && break; sleep 0.5; done
NEW=$(find_pid)
if [ -n "$NEW" ] && sot_socket_open "$BIN" "$SOCKET"; then
    echo "backend restarted: pid $NEW on $SOCKET (binary built $(at "$BIN_MTIME"))"
else
    echo "ERROR: backend did not bind $SOCKET after restart — see $LOG and the daemon's log, $("$BIN" log-path 2>/dev/null || echo sotd.log in its state folder)" >&2; exit 1
fi
