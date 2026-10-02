SOT_LAUNCH_WAIT_S=160  # = rust/protocol/src/ops.rs lease::LAUNCH_WAIT; scripts/tests/installer-state.sh compares them

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

# Write the sotd.service unit for PREFIX from TEMPLATE into DEST.
render_sotd_unit() {  # <prefix> <template> <dest>
    sed -e "s|@SOT_BIN@|$1/bin/sotd|" \
        -e "s|@SOT_APPLY@|$1/bin/sot-apply|" \
        -e "s|@SOT_PROJECT_ROOT@|$HOME|" \
        "$2" > "$3"
}

# Write the all-in-one sot-launch wrapper for PREFIX / TARGET into DEST.
render_sot_launch() {  # <prefix> <target> <dest>
    local prefix="$1" target="$2" dest="$3"
    cat > "$dest" <<EOF
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
# Single apply owner (ADR 0030 Phase C): on systemd installs the apply runs
# ONLY inside ExecStartPre (daemon stopped, whole install, FE binary
# included, flips together); a try-restart triggers it. Launcher-managed
# daemons (macOS / --no-service) are stopped FIRST, then sot-apply runs here.
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
    chmod +x "$dest"
}

# True when SOCKET accepts a connection (or, with an nc that cannot probe a
# UNIX socket, when the socket file exists; it is never removed on that
# evidence).
sot_socket_open() {  # <socket>
    local socket="$1" pid
    [ -S "$socket" ] || return 1
    if command -v nc >/dev/null 2>&1 && nc -h 2>&1 | grep -q -- '-U'; then
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

# Make the backend's socket answer: return 0 once it does, 1 (with the reason
# on stderr) if it cannot. Waits SOT_LAUNCH_WAIT_S for a successor while a
# previous instance is still shutting down. Never removes the socket: the
# daemon unlinks a stale one itself, and an ensure-side rm can delete a
# successor's fresh bind. Never kills a daemon it started: the daemon's own
# lock wait is shorter than this one.
sot_daemon_ensure() {  # <prefix> <sotd-bin> <socket>
    local prefix="$1" sotd_bin="$2" socket="$3" mode=nohup pid="" code="" start now warned=0
    sot_socket_open "$socket" && return 0
    if sot_service_owned "$prefix"; then
        mode=systemd
    else
        nohup "$sotd_bin" --socket "$socket" --project-root "$HOME" --label sot >/tmp/sotd.log 2>&1 </dev/null &
        pid=$!
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
            echo "ERROR: the backend exited ($code) before opening $socket; see /tmp/sotd.log" >&2
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
    else echo "see /tmp/sotd.log" >&2; fi
    return 1
}
