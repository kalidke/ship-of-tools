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
# All-in-one launcher: apply any armed pending update (offline pointer flip,
# fail-open), start the backend on demand if its per-user socket is missing
# (macOS has no service wiring yet; Linux normally has the systemd unit),
# then SUPERVISE the frontend: exit-75 respawn (ADR 0017 on Unix) and
# crash-loop rollback of a just-applied update (ADR 0030 Phase C3).
PENDING="$prefix/updates/pending-$target.json"
MARKER="$prefix/updates/just-applied-$target"
stop_daemon() { pkill -u "\$(id -u)" -f "$prefix/bin/sotd" 2>/dev/null && sleep 1; }
# Single apply owner (ADR 0030 Phase C): on systemd installs the apply runs
# ONLY inside ExecStartPre (daemon stopped, whole install — FE binary
# included — flips together); a try-restart triggers it. Launcher-managed
# daemons (macOS / --no-service) are stopped FIRST, then sot-apply runs here.
apply_pending() {
    [ -f "\$PENDING" ] || return 0
    [ -x "$prefix/bin/sot-apply" ] || return 0
    if command -v systemctl >/dev/null 2>&1 && systemctl --user is-active sotd.service >/dev/null 2>&1; then
        echo "pending update armed — restarting sotd so ExecStartPre applies it" >&2
        systemctl --user try-restart sotd.service || true
    else
        stop_daemon
        APPLY_OUT="\$("$prefix/bin/sot-apply" 2>&1)"
        [ -n "\$APPLY_OUT" ] && printf '%s\n' "\$APPLY_OUT" >&2
    fi
}
apply_pending
SOCKET="\$("$prefix/bin/sotd" session-socket-path sot)"
socket_open() {
    [ -S "\$SOCKET" ] || return 1
    if command -v nc >/dev/null 2>&1 && nc -h 2>&1 | grep -q -- '-U'; then
        nc -U "\$SOCKET" </dev/null >/dev/null 2>&1 &
        pid=\$!
        sleep 1
        if kill -0 "\$pid" 2>/dev/null; then
            kill "\$pid" 2>/dev/null || true
            wait "\$pid" 2>/dev/null || true
            return 0
        fi
        wait "\$pid"
        return \$?
    fi
    # No nc, or an nc without -U (netcat-traditional), cannot probe:
    # the socket file is the best available evidence, and it is never removed
    # on that evidence; the frontend still fails loud if the connect cannot
    # complete.
    return 0
}
start_daemon_if_needed() {
    if ! socket_open; then
        [ -z "\${SOCKET:-}" ] || rm -f -- "\${SOCKET:?}" 2>/dev/null || true
        nohup "$prefix/bin/sotd" --project-root "\$HOME" --label sot >/tmp/sotd.log 2>&1 </dev/null &
        i=0; while [ \$i -lt 40 ]; do socket_open && break; sleep 0.25; i=\$((i+1)); done
        socket_open || { echo "ERROR: backend did not open \$SOCKET; see /tmp/sotd.log" >&2; exit 1; }
    fi
}
start_daemon_if_needed
FAILS=0; ROLLED=0
while :; do
    START="\$(date +%s)"
    "$prefix/bin/sot" --socket "\$SOCKET"
    RC=\$?
    NOW="\$(date +%s)"
    RUNTIME=\$((NOW - START))
    # A healthy run closes the crash-loop health window.
    [ "\$RUNTIME" -ge 60 ] && rm -f "\${MARKER:?}" 2>/dev/null
    if [ "\$RC" -eq 75 ]; then
        # ADR-0017 self-relaunch: pick up any staged update, then respawn.
        apply_pending
        start_daemon_if_needed
        FAILS=0
        continue
    fi
    if [ "\$RC" -ne 0 ] && [ "\$RUNTIME" -le 10 ]; then
        FAILS=\$((FAILS + 1))
        if [ "\$FAILS" -ge 2 ]; then
            # Roll back ONLY inside the just-applied health window — an
            # unrelated crash weeks later must not downgrade a healthy
            # release.
            if [ "\$ROLLED" -eq 0 ] && [ -f "\$MARKER" ] \
               && [ -n "\$(find "\$MARKER" -mmin -30 2>/dev/null)" ]; then
                echo "frontend crash-looped inside the post-update window — rolling back" >&2
                stop_daemon
                [ -x "$prefix/bin/sot-apply" ] && "$prefix/bin/sot-apply" --rollback >&2
                start_daemon_if_needed
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

# Start the backend if its socket is not open, and wait for it.
sot_daemon_ensure() {  # <prefix> <sotd-bin> <socket>
    local prefix="$1" sotd_bin="$2" socket="$3" i
    if ! sot_socket_open "$socket"; then
        [ -z "${socket:-}" ] || rm -f -- "${socket:?}" 2>/dev/null || true
        nohup "$sotd_bin" --project-root "$HOME" --label sot >/tmp/sotd.log 2>&1 </dev/null &
        i=0; while [ $i -lt 40 ]; do sot_socket_open "$socket" && break; sleep 0.25; i=$((i+1)); done
        sot_socket_open "$socket" || { echo "ERROR: backend did not open $socket; see /tmp/sotd.log" >&2; exit 1; }
    fi
}
