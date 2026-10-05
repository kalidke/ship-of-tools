SOT_LAUNCH_WAIT_S=160  # = rust/protocol/src/ops/lease.rs LAUNCH_WAIT; scripts/tests/installer-state.sh compares them
SOT_LOG_KEEP=5               # a start leaves at most this many nohup daemon logs (sot_prune_logs)
SOT_LOG_CAP_BYTES=16777216   # 16MB: the unprotected logs' total a start prunes down to (sot_prune_logs)

# sot-daemon.sh -- the one place the sotd unit, the all-in-one sot-launch
# wrapper and the backend ensure are written down. Sourced by install.sh,
# sot-apply.sh, launch-sot.sh and, through the text render_sot_launch writes,
# by the rendered wrapper itself. sot-apply.sh runs under /bin/sh (dash) and
# macOS ships bash 3.2, so this file is POSIX shell plus `local`: no arrays,
# `[[`, `declare`, `$'...'`, `function`, or `==` in `[`.

# ExecStart's binary path from a unit's text (stdin -> stdout); the same
# two shapes as install.sh's installer_unit_owner_path, which stays there
# because its ownership gate runs before any checkout exists. A test pins
# the two copies equal.
sot_unit_owner_path() {
    sed -n -E 's/^ExecStart=(.*exec ")?([^ "]+)"?.*/\2/p' | head -1
}

# The owner prefix a sot-launch wrapper's content embeds (stdin -> stdout);
# copy of install.sh's installer_wrapper_owner_prefix, pinned equal by test.
sot_wrapper_owner_prefix() {
    sed -n -E \
        -e 's#^PENDING="(.+)/updates/pending-.*"$#\1#p' \
        -e 's#^export SOT_FRONTEND_BIN="(.+)/bin/sot"$#\1#p' \
        -e 's#^exec "(.+)/bin/sot".*#\1#p' \
        | head -1
}

# Write the sotd.service unit for PREFIX from TEMPLATE into DEST. Like every
# rendered file it is written beside DEST, under a name of this shell's own
# ($$), and moved in, so a failed write leaves the old file whole and returns 1.
render_sotd_unit() {  # <prefix> <template> <dest>
    sed -e "s|@SOT_BIN@|$1/bin/sotd|" \
        -e "s|@SOT_APPLY@|$1/bin/sot-apply|" \
        -e "s|@SOT_PROJECT_ROOT@|$HOME|" \
        "$2" > "$3.new.$$" && chmod 0644 "$3.new.$$" && mv -f "$3.new.$$" "$3" || { rm -f "${3:?}.new.$$"; return 1; }
}

# Write the all-in-one sot-launch wrapper for PREFIX / TARGET into DEST.
render_sot_launch() {  # <prefix> <target> <dest>
    local prefix="$1" target="$2" dest="$3"
    cat > "$dest.new.$$" <<EOF || { rm -f "${dest:?}.new.$$"; return 1; }
#!/usr/bin/env bash
# sot-launch: all-in-one
# All-in-one launcher: apply any armed pending update (offline pointer flip,
# fail-open) and re-exec itself so the new wrapper and library run, make
# sure the backend answers before EVERY frontend spawn, then SUPERVISE the
# frontend: exit-75 respawn (ADR 0017 on Unix) and crash-loop rollback of a
# just-applied update (ADR 0030 Phase C3).
PENDING="$prefix/updates/pending-$target.json"
MARKER="$prefix/updates/just-applied-$target"
# The checkout's library, read at run time: a rollback restores the wrapper
# together with the checkout, so the pair stays matched.
LIB="$prefix/repo/current/scripts/lib/sot-daemon.sh"
. "\$LIB" || { echo "ERROR: cannot read \$LIB" >&2; exit 1; }
SOCKET="\$("$prefix/bin/sotd" session-socket-path sot)"
# An owned service is stopped through systemd so its restart policy cannot
# race the stop.
stop_daemon() {
    if sot_service_owned "$prefix"; then
        systemctl --user stop sotd.service >/dev/null 2>&1
    else
        pkill -u "\$(id -u)" -f "$prefix/bin/sotd" 2>/dev/null && sleep 1
    fi
}
# Apply an armed update (ADR 0030 Phase C): an active owned service is
# try-restarted so its ExecStartPre runs sot-apply; otherwise the daemon is
# stopped FIRST, then sot-apply runs here.
# Succeeds only when the pointer is consumed: the caller then re-execs, and
# the consumed pointer is what stops the re-exec'd wrapper doing it again.
apply_pending() {
    [ -f "\$PENDING" ] || return 1
    [ -x "$prefix/bin/sot-apply" ] || return 1
    if sot_service_owned "$prefix" && systemctl --user is-active --quiet sotd.service >/dev/null 2>&1; then
        echo "pending update armed - restarting sotd so ExecStartPre applies it" >&2
        systemctl --user try-restart sotd.service || true
    else
        stop_daemon
        APPLY_OUT="\$("$prefix/bin/sot-apply" 2>&1)"
        [ -n "\$APPLY_OUT" ] && printf '%s\n' "\$APPLY_OUT" >&2
    fi
    [ ! -f "\$PENDING" ]
}
if apply_pending; then exec "\$0" "\$@"; fi
FAILS=0; ROLLED=0
while :; do
    sot_daemon_ensure "$prefix" "$prefix/bin/sotd" "\$SOCKET" || exit 1
    START="\$(date +%s)"
    "$prefix/bin/sot" --socket "\$SOCKET"
    RC=\$?
    NOW="\$(date +%s)"
    RUNTIME=\$((NOW - START))
    # A healthy run closes the crash-loop health window.
    [ "\$RUNTIME" -ge 60 ] && rm -f "\${MARKER:?}" 2>/dev/null
    if [ "\$RC" -eq 75 ]; then
        # ADR-0017 self-relaunch: pick up any staged update, then respawn.
        if apply_pending; then exec "\$0" "\$@"; fi
        FAILS=0
        continue
    fi
    if [ "\$RC" -ne 0 ] && [ "\$RUNTIME" -le 10 ]; then
        FAILS=\$((FAILS + 1))
        if [ "\$FAILS" -ge 2 ]; then
            # Roll back ONLY inside the just-applied health window; an
            # unrelated crash weeks later must not downgrade a healthy
            # release.
            if [ "\$ROLLED" -eq 0 ] && [ -f "\$MARKER" ] \
               && [ -n "\$(find "\$MARKER" -mmin -30 2>/dev/null)" ]; then
                echo "frontend crash-looped inside the post-update window - rolling back" >&2
                stop_daemon
                [ -x "$prefix/bin/sot-apply" ] && "$prefix/bin/sot-apply" --rollback >&2
                ROLLED=1; FAILS=0
                continue
            fi
            exit "\$RC"
        fi
        continue
    fi
    exit "\$RC"
done
EOF
    chmod +x "$dest.new.$$" && mv -f "$dest.new.$$" "$dest" || { rm -f "${dest:?}.new.$$"; return 1; }
}

# True when SOCKET accepts a connection (or, with an nc that cannot probe a
# UNIX socket, when the socket file exists; it is never removed on that
# evidence).
sot_socket_open() {  # <socket>
    local socket="$1" pid
    [ -S "$socket" ] || return 1
    # A case, not a pipeline: under a pipefail caller an nc whose -h exits
    # non-zero would fail `nc -h | grep` and read a stale socket as up.
    if command -v nc >/dev/null 2>&1; then
        case "$(nc -h 2>&1)" in
            *-U*)
                nc -U "$socket" </dev/null >/dev/null 2>&1 &
                pid=$!
                sleep 1
                if kill -0 "$pid" 2>/dev/null; then
                    kill "$pid" 2>/dev/null || true
                    wait "$pid" 2>/dev/null || true
                    return 0
                fi
                wait "$pid"
                return $?
                ;;
        esac
    fi
    # No nc, or an nc without -U (netcat-traditional), cannot probe: the
    # socket file is the best available evidence, and it is never removed on
    # that evidence; the frontend still fails loud if the connect cannot
    # complete.
    return 0
}

# True when this install owns a systemd user unit for the backend: systemctl
# is here, the manifest says systemd, and the unit file install.sh wrote
# points at this prefix's sotd. Reads the FILE, not `systemctl cat`, so apply,
# ensure and rollback ask one question the same way.
sot_service_owned() {  # <prefix>
    command -v systemctl >/dev/null 2>&1 || return 1
    grep -q '"service": *"systemd"' "$1/install.json" 2>/dev/null || return 1
    [ "$(sot_unit_owner_path < "$HOME/.config/systemd/user/sotd.service" 2>/dev/null)" = "$1/bin/sotd" ]
}

# True when LOG is NEWEST or the pid in its name sotd.<stamp>Z-<pid>.log is alive.
sot_log_protected() {  # <log> <newest>
    local p="${1##*/}"
    [ "$1" = "$2" ] && return 0
    p="${p##*Z-}"; p="${p%.log}"
    case "$p" in ''|*[!0-9]*) return 1 ;; esac
    kill -0 "$p" 2>/dev/null
}

# Prune DIR's nohup daemon logs before a start. Oldest first (the legacy fixed
# sotd.log, then sotd.<UTC stamp>Z-<pid>.log by name, which is start order),
# delete while more than SOT_LOG_KEEP remain or the unprotected total exceeds
# SOT_LOG_CAP_BYTES. A log is protected while the <pid> in its name answers
# kill -0 (its writer lives), or while it is the newest (the legacy sotd.log has
# no pid); a protected file is never deleted and is left out of the total. A
# failed rm is logged in one line and skipped; it leaves both bounds.
sot_prune_logs() {  # <dir>
    local dir="$1" f newest="" count total=0 size err
    set --
    [ -f "$dir/sotd.log" ] && set -- "$dir/sotd.log"
    for f in "$dir"/sotd.[0-9]*Z-*.log; do
        [ -f "$f" ] && set -- "$@" "$f"
    done
    count=$#
    # ${1+"$@"}, not "$@": with no logs, bash below 4.1 exits a set -u shell.
    for f in ${1+"$@"}; do newest="$f"; done
    for f in ${1+"$@"}; do
        sot_log_protected "$f" "$newest" && continue
        size="$(wc -c < "$f" 2>/dev/null)" || size=0
        total=$((total + ${size:-0}))
    done
    for f in ${1+"$@"}; do
        [ "$count" -gt "$SOT_LOG_KEEP" ] || [ "$total" -gt "$SOT_LOG_CAP_BYTES" ] || break
        sot_log_protected "$f" "$newest" && continue
        size="$(wc -c < "$f" 2>/dev/null)" || size=0
        err="$(rm -f -- "${f:?}" 2>&1)" || echo "kept old log $f: $err" >&2
        count=$((count - 1))
        total=$((total - ${size:-0}))
    done
    return 0
}

# Make the backend's socket answer: return 0 once it does, 1 (with the reason
# on stderr) if it cannot. Waits SOT_LAUNCH_WAIT_S for a successor while a
# previous instance is still shutting down. Never removes the socket: the
# daemon unlinks a stale one itself, and an ensure-side rm can delete a
# successor's fresh bind. Never kills a daemon it started: the daemon's own
# lock wait is shorter than this one. A backend it starts itself logs under
# the install to a file of its own, <prefix>/logs/sotd.<UTC start>Z-<pid>.log,
# never to a path another user can hold; the newest by name is the current one.
sot_daemon_ensure() {  # <prefix> <sotd-bin> <socket>
    local prefix="$1" sotd_bin="$2" socket="$3" mode=nohup pid="" code="" start now warned=0
    local logdir="$1/logs" logfile="" stamp err
    sot_socket_open "$socket" && return 0
    if sot_service_owned "$prefix"; then
        mode=systemd
    else
        # The logs folder is owner-only, new or old: it keeps every log in it, past and future, from other accounts.
        { mkdir -p "$logdir" && chmod 700 "$logdir"; } || { echo "ERROR: cannot create or secure $logdir" >&2; return 1; }
        sot_prune_logs "$logdir"
        # Milliseconds where date has %N (GNU); 000 where it does not (BSD).
        stamp="$(date -u +%Y%m%d-%H%M%S-%3N)"
        case "$stamp" in *-[0-9][0-9][0-9]) ;; *) stamp="$(date -u +%Y%m%d-%H%M%S)-000" ;; esac
        logfile="$logdir/sotd.${stamp}Z-$$.log"
        # Append, never truncate: the name is new, and a file another daemon
        # still writes is never cut short.
        nohup "$sotd_bin" --socket "$socket" --project-root "$HOME" --label sot >>"$logfile" 2>&1 </dev/null &
        pid=$!
        # Name the log for the daemon that writes it, so a prune keeps it while
        # that daemon lives; a rename never disturbs the open file. On failure
        # this shell's own pid in the old name protects it until the ensure ends.
        if err="$(mv -f -- "$logfile" "$logdir/sotd.${stamp}Z-$pid.log" 2>&1)"; then
            logfile="$logdir/sotd.${stamp}Z-$pid.log"
        else
            echo "kept the log name $logfile: $err" >&2
        fi
    fi
    start="$(date +%s)"
    while :; do
        now="$(date +%s)"
        [ "$now" -lt $((start + SOT_LAUNCH_WAIT_S)) ] || break
        [ "$mode" != systemd ] || systemctl --user start sotd.service >/dev/null 2>&1
        sot_socket_open "$socket" && return 0
        if [ "$mode" = nohup ] && ! kill -0 "$pid" 2>/dev/null; then
            wait "$pid" 2>/dev/null
            code=$?
            # Another daemon may have won the lock.
            sot_socket_open "$socket" && return 0
            echo "ERROR: the backend exited ($code) before opening $socket; see $logfile" >&2
            return 1
        fi
        if [ "$warned" = 0 ] && [ "$now" -ge $((start + 3)) ]; then
            echo "waiting for the backend (a previous one may still be shutting down)" >&2
            warned=1
        fi
        sleep 0.25
    done
    echo "ERROR: the backend did not open $socket within ${SOT_LAUNCH_WAIT_S}s" >&2
    if [ "$mode" = systemd ]; then echo "see journalctl --user -u sotd.service" >&2
    else echo "see $logfile" >&2; fi
    return 1
}

# The one copy helper: copy SRC to DST.new.<pid> (this shell's own, so two
# writers never share it), set MODE when one is given, and move it over DST, so
# DST only ever exists whole. Any failure removes the temp file and returns 1. sot-apply.sh and install.sh carry byte-identical copies
# (they copy before any checkout is known); a test pins all three equal.
sot_install_copy() {  # <src> <dst> [mode]
    if cp -p "$1" "$2.new.$$" && { [ -z "${3:-}" ] || chmod "$3" "$2.new.$$"; } && mv -f "$2.new.$$" "$2"; then
        return 0
    fi
    rm -f "${2:?}.new.$$"
    return 1
}

# Copy FILE to BACKUP, whole or not at all.
sot_backup() {  # <file> <backup>
    sot_install_copy "$1" "$2"
}

# True when the sot-launch wrapper is one of ours and embeds PREFIX.
sot_wrapper_owned() {  # <prefix>
    local wrapper="$HOME/.local/bin/sot-launch"
    [ -f "$wrapper" ] \
       && { grep -q '^# sot-launch: all-in-one$' "$wrapper" || grep -q '^start_daemon_if_needed()' "$wrapper"; } \
       && [ "$(sot_wrapper_owner_prefix < "$wrapper")" = "$1" ]
}

# Back up the unit and the wrapper this install owns, the files
# sot_rerender_owned rewrites, to UNIT-BAK and WRAP-BAK; returns 1 at the
# first that fails.
sot_backup_owned() {  # <prefix> <unit-bak> <wrap-bak>
    if sot_service_owned "$1"; then
        sot_backup "$HOME/.config/systemd/user/sotd.service" "$2" || return 1
    fi
    if sot_wrapper_owned "$1"; then
        sot_backup "$HOME/.local/bin/sot-launch" "$3" || return 1
    fi
    return 0
}

# Re-render, from CHECKOUT's own templates, the unit and the wrapper this
# install owns, so an update carries their text along with the binaries. The
# caller has already taken sot_backup_owned's backups of the same files.
sot_rerender_owned() {  # <prefix> <target> <checkout>
    local prefix="$1" target="$2" checkout="$3"
    local unit="$HOME/.config/systemd/user/sotd.service" wrapper="$HOME/.local/bin/sot-launch"
    if sot_service_owned "$prefix"; then
        render_sotd_unit "$prefix" "$checkout/deploy/sotd.service" "$unit" || return 1
        # The file on disk is right either way; the live check reads NeedDaemonReload.
        timeout 10 systemctl --user daemon-reload >/dev/null 2>&1 \
            || echo "sot-apply: systemctl --user daemon-reload failed (the unit file is current)" >&2
    fi
    if sot_wrapper_owned "$prefix"; then
        render_sot_launch "$prefix" "$target" "$wrapper" || return 1
    fi
    return 0
}
